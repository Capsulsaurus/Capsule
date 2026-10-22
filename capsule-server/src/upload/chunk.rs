//! The chunk rules — the part of the upload contract a deployment may not move.
//!
//! [Transfer — Upload: Chunk Rules and
//! Strictness](../../../capsule-docs/src/content/docs/design/transfer/upload.md) fixes
//! these for a protocol version: the 4 KiB alignment, the 16 MiB ceiling, the required
//! checksum's spelling, and what an offset means. They are constants and pure predicates here
//! rather than fields on [`super::UploadPolicy`] precisely because moving one is a protocol
//! break, not a configuration change — the date in `X-Capsule-Protocol` is what a client
//! checks them against.
//!
//! Everything in this module is total, pure and unit-tested, so the route reads as a sequence
//! of named decisions rather than a sequence of arithmetic.

/// The alignment every chunk but the last must satisfy, in bytes.
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

/// The media type a chunk body must declare.
///
/// The payload is opaque ciphertext, so anything else is a client that has misunderstood what
/// it is sending. TUS v2's `application/partial-upload` is deliberately not accepted: we
/// implement v1 semantics with our own headers, and advertising v2's media type would claim a
/// compatibility we do not have.
pub const CHUNK_MEDIA_TYPE: &str = "application/octet-stream";

/// Where a `PATCH`'s offset sits relative to the session's acknowledged region.
///
/// The three cases are decided by arithmetic alone; distinguishing a *replay* from a
/// *conflict* inside [`Self::Behind`] needs the recorded chunk, which is a store read the
/// route performs — so that distinction is deliberately not made here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetPosition {
    /// Exactly at the acknowledged head: the only offset that may be written.
    AtHead,
    /// At or below an already-acknowledged byte — a replay, a conflict, or a stale offset.
    Behind,
    /// Past the acknowledged head: the client skipped ahead and left a gap.
    Ahead,
}

/// Where `offset` sits for a session that has acknowledged `received_bytes`.
pub fn classify_offset(offset: u64, received_bytes: u64) -> OffsetPosition {
    match offset.cmp(&received_bytes) {
        std::cmp::Ordering::Equal => OffsetPosition::AtHead,
        std::cmp::Ordering::Less => OffsetPosition::Behind,
        std::cmp::Ordering::Greater => OffsetPosition::Ahead,
    }
}

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
/// The header is **required**, because the `(upload_id, offset, chunk_hash)` idempotency
/// tuple is undefined without it: a re-send after a lost acknowledgement could not be told
/// from a client retrying with different bytes.
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

/// Whether a chunk of `len` bytes written at `offset` satisfies the alignment rule for a blob
/// of `total_size` bytes.
///
/// Every chunk except the final one must be a multiple of [`CHUNK_ALIGNMENT`]; the final one
/// is exempt because a blob's total size is arbitrary. "Final" is decided by the declared
/// total, not by the client saying so.
pub fn alignment_ok(offset: u64, len: u64, total_size: u64) -> bool {
    let is_final = offset.saturating_add(len) == total_size;
    is_final || len.is_multiple_of(CHUNK_ALIGNMENT)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_is_at_the_head_behind_or_ahead() {
        assert_eq!(classify_offset(0, 0), OffsetPosition::AtHead);
        assert_eq!(classify_offset(4096, 4096), OffsetPosition::AtHead);
        assert_eq!(classify_offset(0, 4096), OffsetPosition::Behind);
        assert_eq!(classify_offset(8192, 4096), OffsetPosition::Ahead);
    }

    #[test]
    fn an_offset_header_is_read_strictly_or_not_at_all() {
        assert_eq!(parse_offset("0"), Some(0));
        assert_eq!(parse_offset("4096"), Some(4096));
        for bad in [
            "", " 4096", "4096 ", "+4096", "-1", "0x10", "4_096", "4.0", "nine",
        ] {
            assert_eq!(parse_offset(bad), None, "{bad:?} is not an offset");
        }
    }

    #[test]
    fn a_checksum_header_is_a_bare_lowercase_sha256() {
        let good = "a".repeat(64);
        assert_eq!(parse_checksum(&good), Some(good.as_str()));
        assert_eq!(
            parse_checksum(&"0123456789abcdef".repeat(4)),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );

        for bad in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            format!("sha256:{}", "a".repeat(64)),
        ] {
            assert_eq!(parse_checksum(&bad), None, "{bad:?} is not a checksum");
        }
    }

    #[test]
    fn every_chunk_but_the_last_is_aligned() {
        // A 10 000-byte blob sent as 4096 + 4096 + 1808.
        assert!(alignment_ok(0, 4096, 10_000));
        assert!(alignment_ok(4096, 4096, 10_000));
        assert!(
            alignment_ok(8192, 1808, 10_000),
            "the final chunk is exempt"
        );

        // The same blob sent as 4000 + … is a client whose arithmetic has drifted.
        assert!(!alignment_ok(0, 4000, 10_000));

        // A blob smaller than the alignment is one final chunk.
        assert!(alignment_ok(0, 10, 10));
    }

    #[test]
    fn the_chunk_ceiling_is_inclusive() {
        assert!(within_chunk_ceiling(MAX_CHUNK_BYTES));
        assert!(!within_chunk_ceiling(MAX_CHUNK_BYTES + 1));
    }

    #[test]
    fn the_chunk_media_type_ignores_parameters_and_case() {
        assert!(is_chunk_media_type("application/octet-stream"));
        assert!(is_chunk_media_type(
            "application/octet-stream; charset=binary"
        ));
        assert!(is_chunk_media_type("Application/Octet-Stream"));
        assert!(!is_chunk_media_type("application/json"));
        assert!(
            !is_chunk_media_type("application/partial-upload"),
            "TUS v2's media type would claim a compatibility we do not have"
        );
    }

    #[test]
    fn the_suggested_size_follows_the_deployment_tiers() {
        assert_eq!(suggested_chunk_size(1), 256 * 1024);
        assert_eq!(suggested_chunk_size(10 * 1024 * 1024 - 1), 256 * 1024);
        assert_eq!(suggested_chunk_size(10 * 1024 * 1024), 1024 * 1024);
        assert_eq!(suggested_chunk_size(100 * 1024 * 1024), 4 * 1024 * 1024);

        // Whatever the tier, the suggestion is itself a legal non-final chunk — a suggestion
        // the protocol would reject is worse than no suggestion.
        for total in [1, 10 * 1024 * 1024, 100 * 1024 * 1024, u64::MAX] {
            let suggested = suggested_chunk_size(total);
            assert!(suggested.is_multiple_of(CHUNK_ALIGNMENT));
            assert!(within_chunk_ceiling(suggested));
        }
    }

    #[tokio::test]
    async fn one_session_appends_one_chunk_at_a_time_and_the_lock_map_forgets_it() {
        use std::time::Duration;

        use crate::store::UploadId;

        let locks = SessionLocks::default();
        let first_session = UploadId::new("first");
        let second_session = UploadId::new("second");

        let held = locks.acquire(&first_session).await;
        let other = tokio::time::timeout(Duration::from_secs(5), locks.acquire(&second_session))
            .await
            .expect("another session's appends are not held up");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), locks.acquire(&first_session))
                .await
                .is_err(),
            "a second append to the same session waits for the first"
        );

        drop(held);
        let next = tokio::time::timeout(Duration::from_secs(5), locks.acquire(&first_session))
            .await
            .expect("and takes its turn once the first is done");
        assert_eq!(locks.tracked(), 2);

        drop(next);
        drop(other);
        let _sweep = locks.acquire(&UploadId::new("third")).await;
        assert_eq!(
            locks.tracked(),
            1,
            "a session with nothing in flight holds no entry"
        );
        assert_eq!(
            locks
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1,
            "and its dead entry is swept on the next acquisition"
        );
    }

    /// A fresh session in the in-memory doubles, staged and empty, with the record as the
    /// caller read it before taking its turn.
    async fn opened_session(
        name: &str,
        total_size: u64,
    ) -> (
        crate::store::memory::InMemoryUploadSessions,
        crate::blob::memory::InMemoryBlobStore,
        crate::store::memory::ManualClock,
        crate::store::UploadSessionRecord,
    ) {
        use crate::blob::BlobStore as _;
        use crate::store::{Clock as _, UploadSessionStore as _};

        let clock = crate::store::memory::ManualClock::default();
        let sessions = crate::store::memory::InMemoryUploadSessions::with_default_ttl(
            std::sync::Arc::new(clock.clone()),
        );
        let blobs = crate::blob::memory::InMemoryBlobStore::new();
        let record = crate::store::UploadSessionRecord {
            upload_id: crate::store::UploadId::new(name),
            asset_id: crate::store::AssetId::new(format!("{name}-asset")),
            owner_id: crate::store::OwnerId::new("owner"),
            upload_user_id: crate::store::UserId::new("uploader"),
            album_id: None,
            content_type: None,
            expected_hash: "0".repeat(64),
            crypto_suite_id: 1,
            protocol_version: "2026-08-29".to_owned(),
            blob_role: crate::store::upload::BlobRole::Original,
            intent_id: None,
            manifest_envelope: "{}".to_owned(),
            received_bytes: 0,
            total_size,
            status: crate::store::upload::UploadSessionStatus::Pending,
            created_at: clock.now(),
            last_progress_at: clock.now(),
        };
        sessions
            .open(record.clone())
            .await
            .expect("the session opens");
        blobs
            .begin(&record.upload_id)
            .await
            .expect("the stage begins");
        (sessions, blobs, clock, record)
    }

    /// A concurrent chunk completed the session and finalization claimed it while this one waited
    /// its turn. Decided on the stale record, this chunk at the new head would read as
    /// `SizeExceeded` and the session would be abandoned under the finalizer; the post-lock
    /// re-read answers `NotActive`, and the stage is untouched.
    #[tokio::test]
    async fn a_chunk_behind_a_completing_one_finds_the_session_not_active() {
        use crate::blob::BlobStore as _;
        use crate::store::UploadSessionStore as _;

        let (sessions, blobs, clock, stale) = opened_session("completed-while-waiting", 4096).await;
        let id = stale.upload_id.clone();
        let bytes = [7u8; 4096];

        let first = append(&sessions, &blobs, &clock, &stale, 0, &bytes, "a")
            .await
            .expect("the completing chunk lands");
        assert_eq!(
            first,
            Accepted::Advanced {
                next_offset: 4096,
                complete: true
            }
        );
        sessions
            .claim_finalize(&id)
            .await
            .expect("finalization claims the session");

        let late = append(&sessions, &blobs, &clock, &stale, 4096, &bytes, "b")
            .await
            .expect("the late chunk is answered");
        assert_eq!(late, Accepted::NotActive, "not SizeExceeded");
        assert_eq!(
            blobs.staged_len(&id).await.expect("the stage reads"),
            Some(4096),
            "nothing landed on the claimed stage"
        );
    }

    /// A replay says whether the session it lands on is full and unclaimed — the one state in
    /// which the replay is the request left to finish it — and only then.
    #[tokio::test]
    async fn a_replay_names_a_full_session_nobody_has_finished() {
        use crate::store::UploadSessionStore as _;

        let (sessions, blobs, clock, stale) = opened_session("replayed-when-full", 8192).await;
        let id = stale.upload_id.clone();
        let first = [1u8; 4096];
        let last = [2u8; 4096];

        append(&sessions, &blobs, &clock, &stale, 0, &first, "a")
            .await
            .expect("the first chunk lands");
        assert_eq!(
            append(&sessions, &blobs, &clock, &stale, 0, &first, "a")
                .await
                .expect("the replay is answered"),
            Accepted::Replayed {
                next_offset: 4096,
                unfinished: false
            },
            "a session still short of its size is not unfinished, merely in progress"
        );

        append(&sessions, &blobs, &clock, &stale, 4096, &last, "b")
            .await
            .expect("the completing chunk lands");
        for (offset, bytes, hash, next_offset) in [(4096, &last, "b", 8192), (0, &first, "a", 4096)]
        {
            assert_eq!(
                append(&sessions, &blobs, &clock, &stale, offset, bytes, hash)
                    .await
                    .expect("the replay is answered"),
                Accepted::Replayed {
                    next_offset,
                    unfinished: true
                },
                "any replay on a full, unclaimed session names it unfinished"
            );
        }

        sessions
            .claim_finalize(&id)
            .await
            .expect("finalization claims the session");
        assert_eq!(
            append(&sessions, &blobs, &clock, &stale, 4096, &last, "b")
                .await
                .expect("the replay is answered"),
            Accepted::Replayed {
                next_offset: 8192,
                unfinished: false
            },
            "a claimed session has a finisher"
        );
    }

    /// Finalization claimed the session before this chunk's turn came, with the offset still at
    /// the head the caller read. Nothing may land on a stage the finalizer is reading.
    #[tokio::test]
    async fn a_chunk_at_the_head_of_a_claimed_session_lands_nothing() {
        use crate::blob::BlobStore as _;
        use crate::store::UploadSessionStore as _;

        let (sessions, blobs, clock, stale) = opened_session("claimed-while-waiting", 8192).await;
        let id = stale.upload_id.clone();
        sessions
            .claim_finalize(&id)
            .await
            .expect("finalization claims the session");

        let answer = append(&sessions, &blobs, &clock, &stale, 0, &[7u8; 4096], "a")
            .await
            .expect("the chunk is answered");
        assert_eq!(answer, Accepted::NotActive);
        assert_eq!(
            blobs.staged_len(&id).await.expect("the stage reads"),
            Some(0),
            "the stage is untouched"
        );
    }
}

// ===========================================================================================
// The shared append
// ===========================================================================================

/// What appending one chunk did.
///
/// Neutral: the album path and the guest-drop path (`S-C5`) obey invariants 9–12 identically,
/// so the *rules* live here once and each surface maps this outcome onto its own wire shape.
/// Two copies of this logic would be two answers to what an offset means, and the two would
/// drift on exactly the case a client hits after losing a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// Staged and recorded. The session is now at `next_offset`.
    Advanced {
        /// The authoritative offset after this chunk.
        next_offset: u64,
        /// Whether the session has now received every declared byte.
        complete: bool,
    },
    /// An already-accepted chunk was re-sent with the same bytes (invariant 12).
    Replayed {
        /// The offset that chunk already advanced to.
        next_offset: u64,
        /// Whether the session holds every declared byte and nobody has claimed its finish.
        ///
        /// The state a session is left in when the request that completed it went away before
        /// claiming finalization or the deposit — or the process did. The replay is then the
        /// one request that can still finish it, so the caller runs the finish as it would for
        /// [`Accepted::Advanced`] with `complete` set; the claim it takes makes a replay that
        /// races the original request harmless.
        unfinished: bool,
    },
    /// The same offset carried *different* bytes. Corruption, not a retry.
    Conflict,
    /// The offset is not the session's head (invariant 9).
    OffsetMismatch {
        /// Where the session actually is.
        expected: u64,
    },
    /// The chunk is not the declared size and does not end the blob (invariant 11).
    NotAligned,
    /// The chunk would push the blob past its declared length.
    ///
    /// The session is abandoned by the caller on this: a declared size that the bytes exceed is
    /// a client that has lost track of its own upload, and resuming it would keep failing.
    SizeExceeded,
    /// The stage is **shorter** than the session's counter: bytes the server acknowledged are
    /// not on disk. The server's inconsistency, and the one divergence it cannot repair — a
    /// stage that ran *ahead* of the counter is cut back instead (see [`append`]).
    StorageInconsistent,
    /// The blob store is out of space. Nothing was kept, so the same chunk at the same offset
    /// lands once space is freed.
    StorageFull,
    /// The session stopped accepting chunks while this one waited its turn: a concurrent chunk
    /// completed it and finalization has claimed it, or it reached a terminal state.
    NotActive,
    /// The session vanished between the read and the write — expired, discarded, cancelled.
    SessionGone,
}

/// Serialises the appends to one upload session inside this process.
///
/// # Why a lock, when the stage is append-only
///
/// The offset rule is decided from the session as it was *read*, and the read and the write are
/// separate awaits. Two chunks for the same offset arriving together — a client that retried
/// before its first attempt was answered — could both read offset `n`, both pass the rule, and
/// both reach the blob store; the second then met a stage that no longer ended at `n` and the
/// session reported an inconsistency that was really a race. Holding one lock per session across
/// read, write and record makes the second chunk see the first one's outcome: a replay with the
/// same bytes, a conflict with different ones.
///
/// # Why in-process
///
/// A stage is a file on this server's own disk (design/filesystem/server.md), so only this
/// process ever appends to it. A lock in the session store would serialise across processes that
/// cannot share a stage, at the price of a network round trip per chunk.
///
/// # Bounded
///
/// Entries are weak and swept on every acquisition, so the map holds the sessions with a chunk
/// in flight or waiting, never every session that ever took one.
#[derive(Debug, Default)]
pub struct SessionLocks {
    held: std::sync::Mutex<
        std::collections::BTreeMap<crate::store::UploadId, std::sync::Weak<tokio::sync::Mutex<()>>>,
    >,
}

impl SessionLocks {
    /// Wait for, then hold, `upload`'s append lock. Dropping the guard releases it.
    pub async fn acquire(
        &self,
        upload: &crate::store::UploadId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            held.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = held.get(upload).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                held.insert(upload.clone(), std::sync::Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }

    /// How many sessions currently have a lock entry, for a test of the bound.
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|lock| lock.strong_count() > 0)
            .count()
    }
}

/// The process's one set of append locks, shared by the album path and the drop path — both
/// append to the same kind of session through [`append`], so one map is what serialises them.
static APPEND_LOCKS: std::sync::LazyLock<SessionLocks> =
    std::sync::LazyLock::new(SessionLocks::default);

/// Stage one chunk and record it, applying invariants 9–12.
///
/// `record` is the session as it was read; the caller has already authorized the write and
/// checked the session is active, because *who* may append differs between an album upload (the
/// session's uploader) and a drop (whoever holds the link) and that is the one thing the two
/// surfaces genuinely do not share.
///
/// The checksum is verified **before any write**, so a mismatch persists nothing and leaves the
/// offset where it was.
///
/// # Serialised, and decided on a fresh read
///
/// The whole decision runs under the session's [`SessionLocks`] entry, and starts by reading the
/// session again: `record` is what the caller authorized, but the offset it carries may be a
/// concurrent chunk's out-of-date view. Only the identifier is taken from it.
///
/// # A stage that ran ahead is cut back
///
/// A stage longer than the session's counter holds bytes nobody acknowledged — an append that
/// landed and then lost the session write recording it, or one a crash interrupted. The client
/// will send them again at the counter, so the stage is cut back to it and the append retried
/// once, rather than answering `storage_inconsistent` until the session expires. A stage
/// *shorter* than the counter lost acknowledged bytes and stays an inconsistency.
///
/// # Errors
///
/// Returns a [`StoreError`](crate::store::StoreError) only when a collaborator could not answer; every refusal a client
/// can act on is an [`Accepted`] variant.
pub async fn append(
    sessions: &dyn crate::store::UploadSessionStore,
    blobs: &dyn crate::blob::BlobStore,
    clock: &dyn crate::store::Clock,
    record: &crate::store::UploadSessionRecord,
    offset: u64,
    bytes: &[u8],
    received_hash: &str,
) -> Result<Accepted, crate::store::StoreError> {
    let id = &record.upload_id;
    let _turn = APPEND_LOCKS.acquire(id).await;

    let Some(record) = sessions.read(id).await? else {
        return Ok(Accepted::SessionGone);
    };
    let record = &record;

    // Replay, conflict, or a stale offset — all three answered from the accepted-chunk record
    // rather than from the bytes on disk.
    match classify_offset(offset, record.received_bytes) {
        OffsetPosition::Behind => {
            let recorded = sessions.chunk_at(id, offset).await?;
            return Ok(match recorded {
                Some(chunk) if chunk.chunk_hash == received_hash => {
                    let unfinished = record.received_bytes == record.total_size
                        && matches!(
                            record.status,
                            crate::store::UploadSessionStatus::Pending
                                | crate::store::UploadSessionStatus::Uploading
                        );
                    if unfinished {
                        tracing::info!(
                            upload_id = %id,
                            offset,
                            "a chunk was replayed on a full session nobody has finished"
                        );
                    } else {
                        tracing::debug!(upload_id = %id, offset, "an accepted chunk was replayed");
                    }
                    Accepted::Replayed {
                        next_offset: chunk.next_offset,
                        unfinished,
                    }
                }
                Some(_) => Accepted::Conflict,
                None => Accepted::OffsetMismatch {
                    expected: record.received_bytes,
                },
            });
        }
        OffsetPosition::Ahead => {
            return Ok(Accepted::OffsetMismatch {
                expected: record.received_bytes,
            });
        }
        OffsetPosition::AtHead => {}
    }

    // After the replay check, so a duplicate of the chunk that completed the session is still
    // answered as the replay it is; before any write, so nothing lands on a stage finalization
    // has claimed.
    if !record.status.is_active()
        || record.status == crate::store::UploadSessionStatus::WaitingForProcessing
    {
        return Ok(Accepted::NotActive);
    }

    let length = bytes.len() as u64;
    if !alignment_ok(offset, length, record.total_size) {
        return Ok(Accepted::NotAligned);
    }
    if offset.saturating_add(length) > record.total_size {
        tracing::warn!(
            upload_id = %id,
            offset,
            length,
            total = record.total_size,
            "a chunk was refused: it would exceed the declared size"
        );
        return Ok(Accepted::SizeExceeded);
    }

    // Durable before the acknowledgement. The port's own offset cross-check is the append
    // path's storage-consistency guard: a stage whose length is not the session's counter is
    // the server's inconsistency, never the client's fault.
    let mut staged = blobs.append(id, offset, bytes).await;
    if let Err(crate::blob::BlobError::OffsetMismatch { actual, .. }) = staged
        && actual > offset
    {
        // Unacknowledged bytes past the counter. Cut them away and land this chunk where the
        // session says it belongs — once: a stage that is still wrong after a cut is not a stage
        // this call understands.
        tracing::warn!(
            upload_id = %id,
            counter = offset,
            on_disk = actual,
            "the stage ran ahead of the session; cutting it back to the session offset"
        );
        match blobs.truncate_staged(id, offset).await {
            Ok(_) => staged = blobs.append(id, offset, bytes).await,
            Err(error) => staged = Err(error),
        }
    }
    match staged {
        Ok(_) => {}
        Err(crate::blob::BlobError::OffsetMismatch { actual, .. }) => {
            tracing::error!(
                upload_id = %id,
                counter = record.received_bytes,
                on_disk = actual,
                "the stage and the session's counter diverged"
            );
            return Ok(Accepted::StorageInconsistent);
        }
        Err(crate::blob::BlobError::StorageFull { operation }) => {
            tracing::warn!(
                upload_id = %id,
                offset,
                operation,
                "a chunk was refused: the blob store is out of space"
            );
            return Ok(Accepted::StorageFull);
        }
        Err(other) => {
            tracing::error!(%other, upload_id = %id, "the chunk could not be staged");
            return Err(crate::store::StoreError::Unavailable {
                store: "blobs",
                detail: other.to_string(),
            });
        }
    }

    // One write, not three. The byte counter, the progress clock and the replay entry are one
    // fact about one event, and a crash between them used to leave all three disagreeing.
    let Some(updated) = sessions
        .record_progress(
            id,
            crate::store::AcceptedChunk {
                offset,
                chunk_hash: received_hash.to_owned(),
                next_offset: offset + length,
                accepted_at: clock.now(),
            },
        )
        .await?
    else {
        return Ok(Accepted::SessionGone);
    };

    Ok(Accepted::Advanced {
        next_offset: updated.received_bytes,
        complete: updated.received_bytes == updated.total_size,
    })
}
