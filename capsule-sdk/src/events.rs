//! The typed **transfer event stream**: what an upload, a push or a fetch is doing, as it does
//! it, for a caller that renders progress (the TUI's transfer view, a mobile progress bar).
//!
//! Progress used to exist only as `tracing` lines, which a UI cannot consume. A
//! [`TransferEvents`] handle is attached to the clients that move bytes —
//! [`UploadClient::with_events`](crate::upload::UploadClient::with_events) (which
//! [`push_bundle`](crate::push::push_bundle) drives),
//! [`verify_custody_with_events`](crate::push::verify_custody_with_events) and
//! [`RangedFetcher::with_events`](crate::fetch::RangedFetcher::with_events) — and every
//! subscriber receives each [`TransferEvent`] in the order the client took the step.
//!
//! # Contract
//!
//! - **Zero cost when nobody listens.** A client built without a handle, or one whose handle has
//!   no live subscriber, never constructs an event: [`TransferEvents::emit`] takes a closure and
//!   calls it only when a receiver exists. The disabled path is one branch.
//! - **A transfer never waits on its observer.** The channel is a bounded
//!   [`tokio::sync::broadcast`]: a subscriber that falls more than `capacity` events behind
//!   reads [`RecvError::Lagged`](tokio::sync::broadcast::error::RecvError::Lagged) with the count
//!   it missed, and the transfer carries on. Progress is advisory; the transfer's result is the
//!   value its call returns.
//! - **Deterministic order.** One transfer's events arrive in the order its state machine took
//!   the steps, so a scripted fault schedule produces the same sequence every run. Events carry
//!   the blob's content address, so a subscriber watching several concurrent transfers through
//!   one handle can tell them apart.
//!
//! Events describe the transfer, never its contents: offsets, sizes, content addresses and
//! session ids, no plaintext and no key material.

use std::time::Duration;

use serde::Serialize;
use tokio::sync::broadcast;

/// The capacity [`TransferEvents::default_channel`] uses: room for a few seconds of chunk
/// traffic on a fast link before a stalled subscriber starts lagging.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Why a chunk is being sent again, or from a different offset than the last acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RetryReason {
    /// The connection reset or the request was lost; the same chunk is re-sent after `wait`.
    TransportReset {
        /// The backoff taken before the re-send, in milliseconds.
        wait_ms: u64,
    },
    /// The server named a different offset (`offset_mismatch`); the transfer continues from it.
    OffsetRealign {
        /// The authoritative offset the transfer continues from.
        to: u64,
    },
    /// The session was gone (`session_not_found`, or a `HEAD` answering `404`); a new one is
    /// created and the transfer continues from the offset it reports.
    SessionLost,
    /// The server asked the client to wait (`rate_limited`, `unavailable`, `storage_full`);
    /// after `wait_ms` the transfer resumes from the offset `HEAD` reports.
    ServerWait {
        /// The wait taken, in milliseconds.
        wait_ms: u64,
    },
}

/// One step of a transfer.
///
/// `blob` is always the blob's content address (lowercase hex). Offsets and lengths are bytes of
/// ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TransferEvent {
    // ── Upload ──────────────────────────────────────────────────────────────
    /// `POST /upload` opened a session. `resume_offset` is non-zero when the server returned an
    /// existing active session for the same blob (the idempotent re-create).
    SessionCreated {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The declared ciphertext size.
        size: u64,
        /// Where the transfer starts: bytes the server already holds.
        resume_offset: u64,
    },
    /// An existing session was resumed from the offset `HEAD` reported.
    SessionResumed {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The authoritative offset the transfer continues from.
        offset: u64,
    },
    /// A chunk left the client (`PATCH /upload/{id}`).
    ChunkSent {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The chunk's first byte.
        offset: u64,
        /// The chunk's length.
        len: u64,
    },
    /// The server acknowledged a chunk. `acked_offset` is the server's new received-byte count,
    /// which is `offset + len` unless the server kept less (a short write).
    ChunkAcked {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The chunk's first byte.
        offset: u64,
        /// The chunk's length.
        len: u64,
        /// The authoritative offset after the acknowledgement.
        acked_offset: u64,
    },
    /// A chunk will be sent again, or the transfer moves to another offset or session.
    ChunkRetried {
        /// The blob's content address.
        blob: String,
        /// The upload session id the refused chunk was sent to.
        session_id: String,
        /// The refused chunk's first byte.
        offset: u64,
        /// The refused chunk's length.
        len: u64,
        /// Why.
        #[serde(flatten)]
        reason: RetryReason,
    },
    /// The server refused a chunk whose body did not match its `X-Capsule-Checksum`
    /// (`checksum_mismatch`); nothing was persisted and the same chunk is re-sent.
    ChecksumRejected {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The chunk's first byte.
        offset: u64,
        /// The chunk's length.
        len: u64,
    },
    /// Every declared byte reached the server; it is finalizing the blob.
    Finalize {
        /// The blob's content address.
        blob: String,
        /// The session driven to completion.
        session_id: String,
        /// The blob's size.
        size: u64,
    },
    /// The server already held this blob (`duplicate_blob`): resolved as a merge, nothing sent.
    Merged {
        /// The blob's content address.
        blob: String,
        /// The existing asset the reference merges onto.
        asset_ref: String,
    },

    // ── Custody ─────────────────────────────────────────────────────────────
    /// `POST /storage/verify` answered for an asset.
    Verify {
        /// The asset asked about.
        asset_id: String,
        /// The server's aggregate verdict.
        durable: bool,
        /// Blobs declared.
        blobs: u64,
        /// Declared blobs not stored ∧ indexed ∧ retrievable.
        not_durable: u64,
    },
    /// A custody receipt for one blob verified under the pinned attestation keys.
    Receipt {
        /// The asset whose log held it.
        asset_id: String,
        /// The blob's content address.
        blob: String,
        /// The receipt's sequence number in the asset's log.
        receipt_seq: u64,
    },

    // ── Fetch ───────────────────────────────────────────────────────────────
    /// A ranged request for a blob left the client. `max_len` is the bounded window, or `None`
    /// for the open-ended remainder.
    RangeRequested {
        /// The blob's content address.
        blob: String,
        /// The first byte asked for.
        offset: u64,
        /// The most bytes asked for.
        max_len: Option<u64>,
    },
    /// Bytes of a ranged request landed.
    RangeReceived {
        /// The blob's content address.
        blob: String,
        /// The first byte that landed.
        offset: u64,
        /// How many bytes landed.
        len: u64,
    },
    /// A ranged request brought nothing; the fetch backs off and resumes from the same offset.
    /// `stalled` is set when no bytes arrived for the stall bound.
    FetchRetried {
        /// The blob's content address.
        blob: String,
        /// The offset the fetch resumes from.
        offset: u64,
        /// Whether the no-bytes-for-T stall cut tripped.
        stalled: bool,
        /// The backoff taken, in milliseconds.
        wait_ms: u64,
    },
    /// The fetched bytes hashed to the requested content address.
    FetchVerified {
        /// The blob's content address.
        blob: String,
        /// The blob's size.
        size: u64,
    },
    /// The fetched bytes did not hash to the requested content address; they are discarded.
    IntegrityRejected {
        /// The requested content address.
        blob: String,
        /// How many bytes were fetched and discarded.
        size: u64,
    },
}

/// The handle a transfer publishes its [`TransferEvent`]s through.
///
/// [`TransferEvents::disabled`] (the [`Default`]) publishes nothing and costs one branch per
/// step. [`TransferEvents::channel`] publishes to every [`subscribe`](Self::subscribe)r. Cloning
/// the handle shares the channel, so one handle can be attached to several clients.
#[derive(Debug, Clone, Default)]
pub struct TransferEvents {
    sender: Option<broadcast::Sender<TransferEvent>>,
}

impl TransferEvents {
    /// A handle that publishes nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self { sender: None }
    }

    /// A handle over a fresh channel that holds up to `capacity` events per subscriber before a
    /// slow one lags. `capacity` is raised to 1 when zero.
    #[must_use]
    pub fn channel(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self {
            sender: Some(sender),
        }
    }

    /// [`channel`](Self::channel) at [`DEFAULT_CAPACITY`].
    #[must_use]
    pub fn default_channel() -> Self {
        Self::channel(DEFAULT_CAPACITY)
    }

    /// A receiver of every event published from now on, or `None` on a disabled handle.
    #[must_use]
    pub fn subscribe(&self) -> Option<broadcast::Receiver<TransferEvent>> {
        self.sender.as_ref().map(broadcast::Sender::subscribe)
    }

    /// Whether an event published now would reach anyone.
    #[must_use]
    pub fn is_observed(&self) -> bool {
        self.sender
            .as_ref()
            .is_some_and(|sender| sender.receiver_count() > 0)
    }

    /// Publish the event `build` makes, calling it only when someone is subscribed.
    pub fn emit(&self, build: impl FnOnce() -> TransferEvent) {
        let Some(sender) = &self.sender else {
            return;
        };
        if sender.receiver_count() == 0 {
            return;
        }
        let event = build();
        tracing::trace!(?event, "transfer event");
        // A send fails only when every receiver dropped between the count and the send: there
        // is nobody left to tell, which is the case the count already skips.
        let _ = sender.send(event);
    }
}

/// A [`Duration`] as whole milliseconds, saturating — the unit events carry waits in.
pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
