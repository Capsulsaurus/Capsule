//! The chunk rules — the part of the upload contract a deployment may not move — and range
//! admission, the one decision every chunk passes before a byte of it is written.
//!
//! [Transfer — Upload: Chunk Rules and
//! Strictness](../../../capsule-docs/src/content/docs/design/transfer/upload.md) fixes
//! these for a protocol version: the 4 KiB grid, the 16 MiB ceiling, the in-flight cap's
//! ceiling, the fragment bound, and the required checksum's spelling. They are constants and
//! pure predicates here rather than fields on [`super::UploadPolicy`] precisely because moving
//! one is a protocol break, not a configuration change — the date in `X-Capsule-Protocol` is
//! what a client checks them against.
//!
//! # Admission
//!
//! A session's bytes arrive as ranges on the grid, in any order. Each `PATCH` is decided
//! against the session's committed set and the ranges of it still in flight, atomically per
//! session and before anything is written ([`admit`]):
//!
//! | The range is… | Answer |
//! | --- | --- |
//! | wholly inside the committed set | a duplicate: the session's state, nothing written |
//! | overlapping a range in flight | `range_conflict`, with `Retry-After` |
//! | partly committed | `range_conflict` |
//! | one more than the session's in-flight cap | `too_many_in_flight` |
//! | past the fragment bound | `too_fragmented` |
//! | anything else | claimed, written, fsynced, recorded, `204` |
//!
//! No write can therefore touch a committed byte, and two requests never write the same byte at
//! once. The in-flight claims live in this process ([`SessionClaims`]) rather than in the
//! session store: a stage is a file on this server's own disk, so only this process ever writes
//! it, and a claim that died with its process is simply a range nobody recorded.
//!
//! Everything but [`write_range`] is total, pure and unit-tested, so the route reads as a
//! sequence of named decisions rather than a sequence of arithmetic.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crate::store::{CommittedRange, CommittedSet, UploadId, UploadSessionRecord};

/// The grid every range sits on, in bytes: a range's offset is a multiple of it, and so is its
/// length unless it ends at the declared size.
///
/// 4 KiB keeps server-side writes page-aligned and doubles as a tripwire: a client whose
/// offset arithmetic has drifted mis-aligns long before it corrupts anything.
pub const CHUNK_ALIGNMENT: u64 = 4096;

/// The largest chunk the protocol accepts, in bytes.
///
/// Protocol surface. The transport backstop in [`crate::limits`] deliberately sits *above*
/// this, so a breach is answered by this module's coded rejection rather than by a bare
/// framework `413`.
pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// The ceiling on any session's in-flight cap.
///
/// Protocol surface: the server never advertises more as the advice's `ranges` member, and
/// raising it is a protocol change. A session's own cap is fixed at creation, at or under this.
pub const MAX_IN_FLIGHT_RANGES: u8 = 16;

/// The most fragments a session's committed set may hold after coalescing.
///
/// Protocol surface: it bounds `X-Capsule-Committed`, and lowering it is a protocol change.
pub const MAX_FRAGMENTS: usize = 64;

/// The media type a chunk body must declare.
///
/// The payload is opaque ciphertext, so anything else is a client that has misunderstood what
/// it is sending. TUS v2's `application/partial-upload` is deliberately not accepted: we
/// implement v1 semantics with our own headers, and advertising v2's media type would claim a
/// compatibility we do not have.
pub const CHUNK_MEDIA_TYPE: &str = "application/octet-stream";

/// The `X-Capsule-Offset` header's value, or `None` if it is not one.
///
/// Strict on purpose: only ASCII digits, no sign, no whitespace, no `0x`. A header a client
/// spelled loosely is a client whose offset arithmetic is not to be trusted with the next
/// gigabyte.
pub fn parse_offset(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// The `X-Capsule-Checksum` header's value, or `None` if it is not a bare lowercase-hex
/// SHA-256.
///
/// The header is **required**: the server verifies the bytes against it before writing any of
/// them, so a chunk corrupted in transit persists nothing.
pub fn parse_checksum(raw: &str) -> Option<&str> {
    is_sha256_hex(raw).then_some(raw)
}

/// True if `text` is 64 characters of lowercase hex — the spelling every digest crosses the
/// wire in.
pub fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase() && byte <= b'f')
}

/// Whether a range of `len` bytes at `offset` sits on the grid for a blob of `total_size`
/// bytes.
///
/// The offset is always a multiple of [`CHUNK_ALIGNMENT`]; the length is too, unless the range
/// ends exactly at the declared total — a blob's size is arbitrary. "Ends the blob" is decided
/// by the declared total, not by the client saying so.
pub fn alignment_ok(offset: u64, len: u64, total_size: u64) -> bool {
    let ends_the_blob = offset.saturating_add(len) == total_size;
    offset.is_multiple_of(CHUNK_ALIGNMENT) && (ends_the_blob || len.is_multiple_of(CHUNK_ALIGNMENT))
}

/// Whether a chunk is within the protocol's per-chunk ceiling.
pub fn within_chunk_ceiling(len: u64) -> bool {
    len <= MAX_CHUNK_BYTES
}

/// Whether `content_type` names the opaque-bytes media type, parameters ignored.
pub fn is_chunk_media_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case(CHUNK_MEDIA_TYPE))
}

/// The starting chunk size to suggest for a blob of `total_size` bytes.
///
/// A *starting point only*, and explicitly not protocol surface: `capsule-sdk` owns
/// adaptation and may move away from it immediately. The tiers are the Salvo deployment's, so
/// a client's first chunk after the port is the size it was before.
pub fn suggested_chunk_size(total_size: u64) -> u64 {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    match total_size {
        size if size < 10 * MIB => 256 * KIB,
        size if size < 100 * MIB => MIB,
        _ => 4 * MIB,
    }
}

/// What admission decided about one range, before any byte of it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Disjoint from every committed and in-flight range, inside the cap and the fragment
    /// bound: claim it and write it.
    Claim,
    /// Wholly inside the committed set: answer the session's state and write nothing. Bytes are
    /// not compared; the finalize re-hash is what admits bytes to a content address.
    Duplicate,
    /// Overlaps a range in flight (`in_flight`), or is partly committed. Nothing is written.
    RangeConflict {
        /// Whether the overlap is with a range still in flight, which a `Retry-After` covers.
        in_flight: bool,
    },
    /// The session already has its cap of ranges in flight.
    TooManyInFlight,
    /// Admitting it could leave the committed set past [`MAX_FRAGMENTS`].
    TooFragmented {
        /// Whether other ranges of the session are in flight — a claim an abandoned request
        /// still holds, which a `Retry-After` covers.
        in_flight: bool,
    },
}

/// Decide `start..end` against the session's `committed` set, its `in_flight` ranges and its
/// in-flight `cap`.
///
/// The fragment count is the number the committed set would hold with this range merged in,
/// plus one for every *other* range in flight — each of which may commit without touching its
/// neighbours. With nothing else in flight a range that fills a gap therefore never adds a
/// fragment and is always admissible, and a session can never be left above the bound however
/// its in-flight writes end.
pub fn admit(
    committed: &CommittedSet,
    in_flight: &[(u64, u64)],
    cap: u8,
    start: u64,
    end: u64,
) -> Admission {
    if committed.contains(start, end) {
        return Admission::Duplicate;
    }
    if in_flight.iter().any(|&(a, b)| a < end && start < b) {
        return Admission::RangeConflict { in_flight: true };
    }
    if committed.overlaps(start, end) {
        return Admission::RangeConflict { in_flight: false };
    }
    if in_flight.len() >= usize::from(cap) {
        return Admission::TooManyInFlight;
    }
    if committed.fragments_with(start, end) + in_flight.len() > MAX_FRAGMENTS {
        return Admission::TooFragmented {
            in_flight: !in_flight.is_empty(),
        };
    }
    Admission::Claim
}

#[cfg(test)]
mod tests;

// ===========================================================================================
// The shared write
// ===========================================================================================

/// What writing one range did.
///
/// Neutral: the album path and the guest-drop path obey the same admission rules, so the
/// *rules* live here once and each surface maps this outcome onto its own wire shape. Two copies
/// of this logic would be two answers to what a range means, and the two would drift on exactly
/// the case a client hits after losing a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// Written, fsynced and recorded. The session's committed set is now `committed`.
    Committed {
        /// The committed set after this range.
        committed: CommittedSet,
        /// Whether the set is now exactly `[0, size)`.
        complete: bool,
    },
    /// The range was already wholly committed; nothing was written.
    Duplicate {
        /// The session's committed set.
        committed: CommittedSet,
        /// Whether the session holds every declared byte and nobody has claimed its finish.
        ///
        /// The state a session is left in when the request that completed it went away before
        /// claiming finalization or the deposit — or the process did. The duplicate is then the
        /// one request that can still finish it, so the caller runs the finish as it would for
        /// [`Accepted::Committed`] with `complete` set; the claim it takes makes a duplicate
        /// that races the original request harmless.
        unfinished: bool,
    },
    /// The range overlaps a range in flight, or is partly committed. Nothing was written.
    RangeConflict {
        /// The session's committed set, for the client to re-plan from.
        committed: CommittedSet,
        /// Whether the overlap is with a range still in flight.
        in_flight: bool,
    },
    /// The session already has its cap of ranges in flight. Nothing was written.
    TooManyInFlight,
    /// The range would take the committed set past the fragment bound. Nothing was written.
    TooFragmented {
        /// Whether other ranges of the session are in flight.
        in_flight: bool,
    },
    /// The range is off the 4 KiB grid.
    NotAligned,
    /// The range would end past the declared size.
    ///
    /// The session is abandoned by the caller on this: a declared size the bytes exceed is a
    /// client that has lost track of its own upload, and resuming it would keep failing.
    SizeExceeded,
    /// A live session's stage is gone: the server's own inconsistency, never the client's.
    StorageInconsistent,
    /// The blob store is out of space. Nothing was committed, so the same range lands once
    /// space is freed.
    StorageFull,
    /// The session stopped accepting chunks: finalization has claimed it, or it is terminal.
    NotActive,
    /// The session vanished — expired, discarded, cancelled — before or while this range was
    /// written. A range written under a removed session is never recorded.
    SessionGone,
}

/// The per-session state admission is decided under: a turn, and the ranges in flight.
#[derive(Debug, Default)]
struct SessionState {
    /// Held across admission's read-decide-claim, and across a cancellation or a discard, so
    /// none of them interleave.
    turn: Arc<tokio::sync::Mutex<()>>,
    /// The ranges claimed and not yet released, half-open.
    in_flight: Mutex<Vec<(u64, u64)>>,
}

/// The in-flight claims of every session with a range being admitted or written, in this
/// process.
///
/// # Why in-process
///
/// A stage is a file on this server's own disk (design/filesystem/server.md), so only this
/// process ever writes it. A claim in the session store would serialise across processes that
/// cannot share a stage, at the price of a network round trip per range, and would outlive the
/// write it guards when the process died.
///
/// # Bounded
///
/// Entries are weak and swept on every acquisition, so the map holds the sessions with a range
/// in flight or a turn taken, never every session that ever took one.
#[derive(Debug, Default)]
pub struct SessionClaims {
    held: Mutex<BTreeMap<UploadId, Weak<SessionState>>>,
}

/// One session's turn at admission. Dropping it ends the turn.
#[derive(Debug)]
pub struct AdmissionTurn {
    state: Arc<SessionState>,
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

/// A claimed range. Dropping it releases the claim, which [`write_range`] does only once every
/// write issued under it has returned.
#[derive(Debug)]
struct RangeClaim {
    state: Arc<SessionState>,
    range: (u64, u64),
}

impl Drop for RangeClaim {
    fn drop(&mut self) {
        let mut in_flight = self
            .state
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = in_flight.iter().position(|range| *range == self.range) {
            in_flight.swap_remove(at);
        }
    }
}

impl SessionClaims {
    /// Wait for, then hold, `upload`'s admission turn.
    pub async fn turn(&self, upload: &UploadId) -> AdmissionTurn {
        let state = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            held.retain(|_, state| state.strong_count() > 0);
            if let Some(state) = held.get(upload).and_then(Weak::upgrade) {
                state
            } else {
                let state = Arc::new(SessionState::default());
                held.insert(upload.clone(), Arc::downgrade(&state));
                state
            }
        };
        let turn = Arc::clone(&state.turn).lock_owned().await;
        AdmissionTurn { state, _turn: turn }
    }

    /// How many sessions currently have an entry, for a test of the bound.
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|state| state.strong_count() > 0)
            .count()
    }
}

impl AdmissionTurn {
    /// The ranges of this session in flight now.
    fn in_flight(&self) -> Vec<(u64, u64)> {
        self.state
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Claim `range`. The claim outlives the turn.
    fn claim(&self, range: (u64, u64)) -> RangeClaim {
        self.state
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(range);
        RangeClaim {
            state: Arc::clone(&self.state),
            range,
        }
    }
}

/// The process's one set of claims, shared by the album path and the drop path — both write the
/// same kind of session through [`write_range`], and cancel or discard it through
/// [`admission_turn`], so one map is what serialises them.
static CLAIMS: std::sync::LazyLock<SessionClaims> =
    std::sync::LazyLock::new(SessionClaims::default);

/// Wait for, then hold, `upload`'s admission turn in this process.
///
/// A cancellation and a discard take it around their removal of the session, so no range is
/// admitted against a session being removed; a range already in flight then fails to record and
/// its write is discarded with the stage.
pub async fn admission_turn(upload: &UploadId) -> AdmissionTurn {
    CLAIMS.turn(upload).await
}

/// Admit, write and record one range of `record`'s session.
///
/// `record` is the session as it was read; the caller has already authorized the write,
/// verified the checksum, and checked the session is active, because *who* may write differs
/// between an album upload (the session's uploader) and a drop (whoever holds the link) and that
/// is the one thing the two surfaces genuinely do not share.
///
/// # Decided on a fresh read, under the session's turn
///
/// Admission runs under the session's [`AdmissionTurn`] and starts by reading the session
/// again: `record` is what the caller authorized, but its committed set may be a concurrent
/// range's out-of-date view. Only the identifier is taken from it. The turn ends once the range
/// is claimed, so admitted ranges of one session are written concurrently.
///
/// # The claim outlives the request
///
/// The claim is released only when this function returns, after the write and the record both
/// have, so a write abandoned by its request can never land after a retry of the same range
/// commits. A caller must therefore run this to completion — both routes run it as a task of
/// its own — rather than drop it mid-write.
///
/// # Errors
///
/// Returns a [`StoreError`](crate::store::StoreError) only when a collaborator could not answer;
/// every refusal a client can act on is an [`Accepted`] variant.
pub async fn write_range(
    sessions: &dyn crate::store::UploadSessionStore,
    blobs: &dyn crate::blob::BlobStore,
    clock: &dyn crate::store::Clock,
    record: &UploadSessionRecord,
    offset: u64,
    bytes: &[u8],
) -> Result<Accepted, crate::store::StoreError> {
    let id = &record.upload_id;
    let length = bytes.len() as u64;
    let end = offset.saturating_add(length);

    let claim = {
        let turn = admission_turn(id).await;
        let Some(record) = sessions.read(id).await? else {
            return Ok(Accepted::SessionGone);
        };
        let in_flight = turn.in_flight();
        let admission = admit(
            &record.committed,
            &in_flight,
            record.in_flight_cap,
            offset,
            end,
        );

        // A duplicate first, so a re-send of the range that completed the session is still
        // answered as the duplicate it is; the other refusals before the status check, because
        // none of them writes anything either.
        match admission {
            Admission::Duplicate => {
                let unfinished = record.is_full_and_unclaimed();
                if unfinished {
                    tracing::info!(upload_id = %id, offset, end, "a duplicate range met a full session nobody has finished");
                } else {
                    tracing::debug!(upload_id = %id, offset, end, "a committed range was sent again");
                }
                return Ok(Accepted::Duplicate {
                    committed: record.committed,
                    unfinished,
                });
            }
            Admission::RangeConflict { in_flight } => {
                tracing::info!(upload_id = %id, offset, end, in_flight, "a range was refused: it overlaps one committed or in flight");
                return Ok(Accepted::RangeConflict {
                    committed: record.committed,
                    in_flight,
                });
            }
            Admission::Claim | Admission::TooManyInFlight | Admission::TooFragmented { .. } => {}
        }

        // Before any write, so nothing lands on a stage finalization has claimed.
        if !record.status.is_evictable() {
            return Ok(Accepted::NotActive);
        }
        if !alignment_ok(offset, length, record.total_size) {
            return Ok(Accepted::NotAligned);
        }
        if end > record.total_size {
            tracing::warn!(
                upload_id = %id,
                offset,
                length,
                total = record.total_size,
                "a range was refused: it would exceed the declared size"
            );
            return Ok(Accepted::SizeExceeded);
        }
        match admission {
            Admission::TooManyInFlight => {
                tracing::info!(upload_id = %id, offset, end, cap = record.in_flight_cap, "a range was refused: the session's in-flight cap is full");
                return Ok(Accepted::TooManyInFlight);
            }
            Admission::TooFragmented { in_flight } => {
                tracing::info!(upload_id = %id, offset, end, fragments = record.committed.fragments(), in_flight, "a range was refused: it would pass the fragment bound");
                return Ok(Accepted::TooFragmented { in_flight });
            }
            _ => {}
        }
        tracing::trace!(upload_id = %id, offset, end, in_flight = in_flight.len(), "admitted a range");
        turn.claim((offset, end))
    };

    // Durable before the record: the record is the commit point.
    match blobs.write_staged(id, offset, bytes).await {
        Ok(_) => {}
        Err(crate::blob::BlobError::NotStaged { .. }) => {
            // A cancellation or a discard removed the stage under this write; the session is
            // gone too, or is about to be. Otherwise a live session lost its stage.
            drop(claim);
            return Ok(match sessions.read(id).await? {
                Some(record) if record.status.is_evictable() => {
                    tracing::error!(upload_id = %id, offset, "a live session's stage is gone");
                    Accepted::StorageInconsistent
                }
                Some(_) => Accepted::NotActive,
                None => Accepted::SessionGone,
            });
        }
        Err(crate::blob::BlobError::StorageFull { operation }) => {
            tracing::warn!(
                upload_id = %id,
                offset,
                operation,
                "a range was refused: the blob store is out of space"
            );
            return Ok(Accepted::StorageFull);
        }
        Err(other) => {
            tracing::error!(%other, upload_id = %id, offset, "the range could not be staged");
            return Err(crate::store::StoreError::Unavailable {
                store: "blobs",
                detail: other.to_string(),
            });
        }
    }

    let recorded = sessions
        .record_range(
            id,
            CommittedRange {
                start: offset,
                end,
                accepted_at: clock.now(),
            },
        )
        .await;
    drop(claim);
    let Some(updated) = recorded? else {
        tracing::info!(upload_id = %id, offset, end, "a written range found its session gone; it was not recorded");
        return Ok(Accepted::SessionGone);
    };

    tracing::debug!(
        upload_id = %id,
        offset,
        end,
        fragments = updated.committed.fragments(),
        prefix = updated.committed.prefix(),
        "committed a range"
    );
    let complete = updated.is_full();
    Ok(Accepted::Committed {
        committed: updated.committed,
        complete,
    })
}
