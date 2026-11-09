//! The **transfer event stream** across the FFI boundary: the flattened mirror of
//! [`crate::events`] and the subscription an app's real-time transfer view polls.
//!
//! # Shape
//!
//! A subscription is an object with one `async` method, [`FfiTransferSubscription::next`],
//! rather than a foreign callback. The app pulls at its own pace, so a view that stops
//! drawing stops pulling and nothing on the Rust side calls into foreign code from a transfer
//! task; cancelling the foreign task that awaits `next` drops the future and ends the wait.
//! The transfer itself never waits on the observer: the channel is the bounded broadcast
//! [`TransferEvents`] already is, and a subscriber that falls behind reads
//! [`FfiTransferFeedItem::Lagged`] with the count it missed.
//!
//! # Stamps
//!
//! Each event is stamped, as it is received, with the milliseconds since the subscription
//! opened on a monotonic clock ([`std::time::Instant`]). The stamp is taken here so the fold
//! that draws the stream (the apps' `TransferBoard`, the TUI's transfer view) never reads a
//! clock, and a recorded stream replays into exactly the frames it drew live.
//!
//! Events describe the transfer, never its contents: offsets, sizes, content addresses and
//! session ids, no plaintext and no key material.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::{self};

use crate::events::{RetryReason, TransferEvent, TransferEvents, millis};

/// Why an upload chunk is sent again. Mirrors [`RetryReason`] variant for variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRetryReason {
    /// The connection reset or the request was lost; the same chunk is re-sent after the wait.
    TransportReset {
        /// The backoff taken before the re-send, in milliseconds.
        wait_ms: u64,
    },
    /// The server named a different offset; the transfer continues from it.
    OffsetRealign {
        /// The authoritative offset the transfer continues from.
        to: u64,
    },
    /// The session was gone; a new one is created and the transfer continues from the offset
    /// it reports.
    SessionLost,
    /// The server asked the client to wait; afterwards the transfer resumes from the offset
    /// `HEAD` reports.
    ServerWait {
        /// The wait taken, in milliseconds.
        wait_ms: u64,
    },
}

impl From<RetryReason> for FfiRetryReason {
    fn from(reason: RetryReason) -> Self {
        match reason {
            RetryReason::TransportReset { wait_ms } => Self::TransportReset { wait_ms },
            RetryReason::OffsetRealign { to } => Self::OffsetRealign { to },
            RetryReason::SessionLost => Self::SessionLost,
            RetryReason::ServerWait { wait_ms } => Self::ServerWait { wait_ms },
        }
    }
}

/// One step of an upload, a custody check or a ranged fetch. Mirrors [`TransferEvent`]
/// variant for variant and field for field; `blob` is the content address (lowercase hex) and
/// offsets and lengths are bytes of ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTransferEvent {
    /// `POST /upload` opened a session; `resume_offset` is non-zero when the server returned an
    /// existing session for the same blob.
    SessionCreated {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The declared ciphertext size.
        size: u64,
        /// Bytes the server already holds.
        resume_offset: u64,
    },
    /// An existing session resumed from the offset `HEAD` reported.
    SessionResumed {
        /// The blob's content address.
        blob: String,
        /// The upload session id.
        session_id: String,
        /// The declared ciphertext size.
        size: u64,
        /// The authoritative offset the transfer continues from.
        offset: u64,
    },
    /// A chunk left the client.
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
    /// The server acknowledged a chunk; `acked_offset` is its new received-byte count.
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
        reason: FfiRetryReason,
    },
    /// The server refused a chunk whose body did not match its checksum; it is re-sent.
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
    /// The server already held the blob: resolved as a merge, nothing sent.
    Merged {
        /// The blob's content address.
        blob: String,
        /// The existing asset the reference merges onto.
        asset_ref: String,
    },
    /// `POST /storage/verify` answered for an asset.
    Verify {
        /// The asset asked about.
        asset_id: String,
        /// The server's aggregate verdict.
        durable: bool,
        /// Blobs declared.
        blobs: u64,
        /// Declared blobs not stored, indexed and retrievable.
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
    /// A ranged request left the client; `max_len` is the bounded window, or `None` for the
    /// open-ended remainder.
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
    FetchRetried {
        /// The blob's content address.
        blob: String,
        /// The offset the fetch resumes from.
        offset: u64,
        /// Whether the no-bytes stall cut tripped.
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
    /// The fetched bytes did not hash to the requested content address and were discarded.
    IntegrityRejected {
        /// The requested content address.
        blob: String,
        /// How many bytes were fetched and discarded.
        size: u64,
    },
}

impl From<TransferEvent> for FfiTransferEvent {
    fn from(event: TransferEvent) -> Self {
        match event {
            TransferEvent::SessionCreated {
                blob,
                session_id,
                size,
                resume_offset,
            } => Self::SessionCreated {
                blob,
                session_id,
                size,
                resume_offset,
            },
            TransferEvent::SessionResumed {
                blob,
                session_id,
                size,
                offset,
            } => Self::SessionResumed {
                blob,
                session_id,
                size,
                offset,
            },
            TransferEvent::ChunkSent {
                blob,
                session_id,
                offset,
                len,
            } => Self::ChunkSent {
                blob,
                session_id,
                offset,
                len,
            },
            TransferEvent::ChunkAcked {
                blob,
                session_id,
                offset,
                len,
                acked_offset,
            } => Self::ChunkAcked {
                blob,
                session_id,
                offset,
                len,
                acked_offset,
            },
            TransferEvent::ChunkRetried {
                blob,
                session_id,
                offset,
                len,
                reason,
            } => Self::ChunkRetried {
                blob,
                session_id,
                offset,
                len,
                reason: reason.into(),
            },
            TransferEvent::ChecksumRejected {
                blob,
                session_id,
                offset,
                len,
            } => Self::ChecksumRejected {
                blob,
                session_id,
                offset,
                len,
            },
            TransferEvent::Finalize {
                blob,
                session_id,
                size,
            } => Self::Finalize {
                blob,
                session_id,
                size,
            },
            TransferEvent::Merged { blob, asset_ref } => Self::Merged { blob, asset_ref },
            TransferEvent::Verify {
                asset_id,
                durable,
                blobs,
                not_durable,
            } => Self::Verify {
                asset_id,
                durable,
                blobs,
                not_durable,
            },
            TransferEvent::Receipt {
                asset_id,
                blob,
                receipt_seq,
            } => Self::Receipt {
                asset_id,
                blob,
                receipt_seq,
            },
            TransferEvent::RangeRequested {
                blob,
                offset,
                max_len,
            } => Self::RangeRequested {
                blob,
                offset,
                max_len,
            },
            TransferEvent::RangeReceived { blob, offset, len } => {
                Self::RangeReceived { blob, offset, len }
            }
            TransferEvent::FetchRetried {
                blob,
                offset,
                stalled,
                wait_ms,
            } => Self::FetchRetried {
                blob,
                offset,
                stalled,
                wait_ms,
            },
            TransferEvent::FetchVerified { blob, size } => Self::FetchVerified { blob, size },
            TransferEvent::IntegrityRejected { blob, size } => {
                Self::IntegrityRejected { blob, size }
            }
        }
    }
}

/// A transfer event and when it arrived: milliseconds since the subscription opened, on a
/// monotonic clock.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiStampedTransferEvent {
    /// Milliseconds since the subscription opened.
    pub at_ms: u64,
    /// The event.
    pub event: FfiTransferEvent,
}

/// What one pull of a subscription yields.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTransferFeedItem {
    /// The next event.
    Event {
        /// The event and its stamp.
        stamped: FfiStampedTransferEvent,
    },
    /// The subscriber fell behind and this many events never arrived. The transfers carried
    /// on; only their progress went unseen.
    Lagged {
        /// How many events were missed.
        missed: u64,
    },
}

/// The time since a subscription opened, read when an event arrives.
type Elapsed = Arc<dyn Fn() -> Duration + Send + Sync>;

/// A live subscription to a session's transfer events: every step published after it opened.
///
/// Obtained from [`FfiSession::transfer_events`](crate::ffi::FfiSession::transfer_events).
/// Pull with [`next`](Self::next) until it answers `None`, which it does once the session that
/// publishes the events is gone.
#[derive(uniffi::Object)]
pub struct FfiTransferSubscription {
    receiver: Mutex<Option<broadcast::Receiver<TransferEvent>>>,
    elapsed: Elapsed,
}

impl FfiTransferSubscription {
    /// Subscribe to `events`, stamping each event with the time since now.
    pub(crate) fn open(events: &TransferEvents) -> Arc<Self> {
        let opened = Instant::now();
        Self::with_clock(events, Arc::new(move || opened.elapsed()))
    }

    /// Subscribe to `events`, stamping each event with what `elapsed` reads when it arrives.
    /// The seam a deterministic test drives.
    pub(crate) fn with_clock(events: &TransferEvents, elapsed: Elapsed) -> Arc<Self> {
        let receiver = events.subscribe();
        tracing::debug!(
            observed = receiver.is_some(),
            "ffi: transfer event subscription opened"
        );
        Arc::new(Self {
            receiver: Mutex::new(receiver),
            elapsed,
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl FfiTransferSubscription {
    /// The next event, or how many were missed since the last pull; `None` once the session
    /// publishing them is gone, and from then on.
    ///
    /// Waits for an event when none is pending. Concurrent pulls take turns: each event is
    /// delivered to exactly one of them.
    pub async fn next(&self) -> Option<FfiTransferFeedItem> {
        let mut guard = self.receiver.lock().await;
        let receiver = guard.as_mut()?;
        match receiver.recv().await {
            Ok(event) => Some(FfiTransferFeedItem::Event {
                stamped: FfiStampedTransferEvent {
                    at_ms: millis((self.elapsed)()),
                    event: event.into(),
                },
            }),
            Err(RecvError::Lagged(missed)) => {
                tracing::debug!(missed, "ffi: transfer event subscriber lagged");
                Some(FfiTransferFeedItem::Lagged { missed })
            }
            Err(RecvError::Closed) => {
                tracing::debug!("ffi: transfer event stream closed");
                *guard = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
