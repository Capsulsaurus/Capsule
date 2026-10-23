//! The real-time transfer view's model: what every upload and fetch is doing, folded from the
//! SDK's transfer event stream ([`capsule_sdk::events`]) and nothing else.
//!
//! [`TransferBoard`] is a pure fold. Each [`Stamped`] event carries the milliseconds since the
//! view started watching, stamped by the runtime as the event arrived, and a [`TransferBoard::tick`]
//! moves the board's clock forward between events; the board never reads a clock itself. So a
//! recorded stream ([`parse_recording`]) replays into exactly the frames it drew live, and a test
//! pins those frames with snapshots.
//!
//! What the board derives from the stream:
//!
//! - **Progress.** Uploads and ranged fetches both move a prefix of the blob: the server's
//!   acknowledged offset, or the bytes a fetch has landed. [`Transfer::done`] is that prefix.
//!   [`Transfer::held`] is the part the server already held before this run sent anything (a
//!   resume after a cut).
//! - **The chunk in flight**, and whether it is being retried or was refused for a checksum
//!   mismatch ([`ActiveChunk`]). A chunk that needed a retry or was refused leaves a [`Scar`],
//!   so the grid still shows where trouble was after the chunk lands.
//! - **Adaptive chunk size.** The length of each non-final upload chunk sent; a change of length
//!   is the adaptive strategy resizing. A fetch's window is fixed for its run.
//! - **Throughput** over the last [`THROUGHPUT_WINDOW_MS`] of stream time.
//! - **Custody.** Verdicts and receipts; a receipt for an uploaded blob marks it verified.
//!
//! The board stays bounded over a long sync, whether or not transfers ever finish:
//!
//! - finished transfers beyond [`FINISHED_KEPT`] are dropped oldest first;
//! - an unfinished transfer (a failed upload, a deferred blob with no receipt, a failed custody
//!   check, a receipt lost to lag) that has had no event for [`STALE_AFTER_MS`] of stream time is
//!   dropped; a later event for its blob starts a fresh entry;
//! - at most [`TRANSFERS_KEPT`] transfers are held in all: past that, the one silent longest goes;
//! - scars per transfer beyond [`SCARS_KEPT`], and throughput samples older than the window.
//!
//! Age is measured on the stream's own stamps and ticks, never a wall clock, so a replay ages
//! entries exactly as the live run did.

pub mod view;

use std::collections::VecDeque;
use std::time::Duration;

use capsule_sdk::events::{RetryReason, TransferEvent, TransferEvents};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time::Instant;

use super::state::Action;

/// How often the transfer screen redraws while it is open: 60 Hz. Events between two frames are
/// folded into the board without a draw each, so a fast link never makes the loop draw faster.
pub const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// The span of stream time throughput is averaged over.
pub const THROUGHPUT_WINDOW_MS: u64 = 2_000;

/// Finished transfers (verified, merged, rejected) kept for display; older ones are dropped.
pub const FINISHED_KEPT: usize = 32;

/// Stream time an unfinished transfer may go without an event before it is dropped: nothing
/// still moving is silent that long, so what is left never got a terminal event.
pub const STALE_AFTER_MS: u64 = 300_000;

/// Transfers held in all, finished or not; the one silent longest is dropped past this.
pub const TRANSFERS_KEPT: usize = 256;

/// Scars kept per transfer; older ones are dropped.
pub const SCARS_KEPT: usize = 64;

/// Distinct chunk sizes remembered per transfer, the current one last.
pub const CHUNK_SIZES_KEPT: usize = 4;

/// One transfer event and when it arrived: milliseconds since the view started watching.
///
/// One line of a recording: the event's own JSON with an `at_ms` field beside its tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamped {
    /// Milliseconds since the view started watching, from the runtime's monotonic clock.
    pub at_ms: u64,
    /// The event.
    #[serde(flatten)]
    pub event: TransferEvent,
}

/// Parse a recorded stream: one [`Stamped`] JSON object per line, blank lines ignored.
///
/// # Errors
///
/// A line that is not a stamped event, as an English message naming the line (1-based).
pub fn parse_recording(text: &str) -> Result<Vec<Stamped>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", index + 1))
        })
        .collect()
}

/// The runtime's subscription to the transfer event stream: each event stamped with the
/// monotonic milliseconds since the feed opened, as the [`Action`] that folds it in.
///
/// The stamp is taken here, in the shell, so the reducer stays clock-free. A feed over a
/// disabled handle, or one whose channel closed, never yields.
#[derive(Debug)]
pub struct Feed {
    receiver: Option<broadcast::Receiver<TransferEvent>>,
    origin: Instant,
}

impl Feed {
    /// Subscribe to `events` from now on.
    #[must_use]
    pub fn new(events: &TransferEvents) -> Self {
        Self {
            receiver: events.subscribe(),
            origin: Instant::now(),
        }
    }

    /// Milliseconds since the feed opened: the clock [`Action::Tick`] and every stamp share.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The next event as an action, or a lag report. Pends forever once there is nothing left
    /// to receive. Cancel-safe: a `select!` that drops it loses no event.
    pub async fn next(&mut self) -> Action {
        loop {
            let Some(receiver) = &mut self.receiver else {
                return std::future::pending().await;
            };
            match receiver.recv().await {
                Ok(event) => {
                    return Action::Transfer(Stamped {
                        at_ms: self.now_ms(),
                        event,
                    });
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    return Action::TransfersLagged(count);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!(target: "capsule_cli::tui", "transfer event channel closed");
                    self.receiver = None;
                }
            }
        }
    }
}

/// Which way the bytes move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// To the server: an upload session.
    Upload,
    /// From the server: a ranged fetch.
    Fetch,
}

/// What the chunk in flight is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkState {
    /// Sent (or requested) and not yet answered.
    InFlight,
    /// Refused or lost; it is sent again after a backoff.
    Retrying,
    /// The server refused it for a checksum mismatch; it is sent again.
    Rejected,
}

/// The one chunk a transfer has in flight: the SDK sends sequentially.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveChunk {
    /// The chunk's first byte.
    pub offset: u64,
    /// Its length, or `None` for an open-ended fetch of the remainder.
    pub len: Option<u64>,
    /// What it is doing.
    pub state: ChunkState,
}

/// What went wrong with a chunk that has since moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScarKind {
    /// It needed a retry.
    Retried,
    /// It was refused for a checksum mismatch.
    Rejected,
}

/// Where trouble was: a byte range and what happened there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scar {
    /// The range's first byte.
    pub offset: u64,
    /// Its length, or `None` to the end of the blob.
    pub len: Option<u64>,
    /// What happened.
    pub kind: ScarKind,
}

/// Why the last retry happened, as the stream said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryNote {
    /// An upload chunk was retried.
    Upload(RetryReason),
    /// A ranged fetch brought nothing and backs off.
    Fetch {
        /// No bytes arrived for the stall bound.
        stalled: bool,
        /// The backoff, in milliseconds.
        wait_ms: u64,
    },
}

/// Where a transfer is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Moving bytes, or about to.
    Active,
    /// Every byte of an upload reached the server; it is finalizing the blob.
    Finalizing,
    /// A custody receipt verified the upload, or the fetched bytes hashed to the address.
    Verified,
    /// The server already held the blob; nothing was sent.
    Merged,
    /// The fetched bytes did not hash to the requested address and were discarded.
    IntegrityRejected,
}

impl Phase {
    /// No further event changes this transfer; a new one for the same blob starts afresh.
    #[must_use]
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Verified | Self::Merged | Self::IntegrityRejected
        )
    }
}

/// One cell of a transfer's chunk grid: what the bytes it covers are doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Not sent yet.
    Pending,
    /// Held by the server before this run sent anything.
    Held,
    /// Acknowledged (or landed, for a fetch).
    Acked,
    /// Acknowledged after a retry.
    AckedAfterRetry,
    /// Acknowledged after a checksum refusal.
    AckedAfterReject,
    /// Verified: a custody receipt, or a matching hash.
    Verified,
    /// In flight now.
    InFlight,
    /// Being retried now.
    Retrying,
    /// Refused for a checksum mismatch, or the whole fetch failed its hash.
    Rejected,
}

/// Everything the view shows about one transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    /// The blob's content address.
    pub blob: String,
    /// Which way the bytes move.
    pub direction: Direction,
    /// The current upload session, once one is open.
    pub session_id: Option<String>,
    /// The blob's size, once the stream has said it.
    pub size: Option<u64>,
    /// The authoritative prefix: bytes acknowledged (upload) or landed (fetch).
    pub done: u64,
    /// The prefix the server held before this run sent anything.
    pub held: u64,
    /// The chunk in flight.
    pub active: Option<ActiveChunk>,
    /// Where chunks needed a retry or were refused, newest last.
    pub scars: Vec<Scar>,
    /// Distinct upload sessions it ran in, created or resumed (more than one after a lost
    /// session).
    pub sessions: u32,
    /// Times the transfer resumed from bytes the server already held.
    pub resumes: u32,
    /// The offset of the last resume.
    pub resumed_at: Option<u64>,
    /// Retries taken.
    pub retries: u32,
    /// Checksum refusals (upload) or failed hashes (fetch).
    pub rejects: u32,
    /// Why the last retry happened.
    pub last_retry: Option<RetryNote>,
    /// Distinct chunk sizes, the current one last.
    pub chunk_sizes: Vec<u64>,
    /// How many times the chunk size changed.
    pub chunk_size_changes: u32,
    /// Bytes moved by this run.
    pub moved: u64,
    /// When the first event for it arrived.
    pub started_ms: u64,
    /// When the last event for it arrived.
    pub last_ms: u64,
    /// The receipt that verified an upload.
    pub receipt_seq: Option<u64>,
    /// Where it is in its life.
    pub phase: Phase,
    /// `(at_ms, bytes)` moved, within the throughput window.
    samples: VecDeque<(u64, u64)>,
}

impl Transfer {
    fn new(blob: &str, direction: Direction, at_ms: u64) -> Self {
        Self {
            blob: blob.to_owned(),
            direction,
            session_id: None,
            size: None,
            done: 0,
            held: 0,
            active: None,
            scars: Vec::new(),
            sessions: 0,
            resumes: 0,
            resumed_at: None,
            retries: 0,
            rejects: 0,
            last_retry: None,
            chunk_sizes: Vec::new(),
            chunk_size_changes: 0,
            moved: 0,
            started_ms: at_ms,
            last_ms: at_ms,
            receipt_seq: None,
            phase: Phase::Active,
            samples: VecDeque::new(),
        }
    }

    /// Bytes per second over the [`THROUGHPUT_WINDOW_MS`] ending at `now_ms`: the window is
    /// `(now - window, now]`, or everything before the clock has run a whole window.
    #[must_use]
    pub fn throughput(&self, now_ms: u64) -> u64 {
        let bytes: u64 = self
            .samples
            .iter()
            .filter(|(at, _)| in_window(*at, now_ms))
            .map(|(_, bytes)| *bytes)
            .sum();
        bytes.saturating_mul(1000) / THROUGHPUT_WINDOW_MS
    }

    /// The current chunk size, once a non-final chunk has been sent.
    #[must_use]
    pub fn chunk_size(&self) -> Option<u64> {
        self.chunk_sizes.last().copied()
    }

    /// The chunk grid in `cells` cells (fewer when the blob has fewer bytes), or `None` while
    /// the size is unknown or zero.
    #[must_use]
    pub fn cells(&self, cells: usize) -> Option<Vec<Cell>> {
        let size = self.size.filter(|size| *size > 0)?;
        let count = u64::try_from(cells).unwrap_or(u64::MAX).min(size);
        let bound = |i: u64| {
            u64::try_from(u128::from(size) * u128::from(i) / u128::from(count)).unwrap_or(size)
        };
        Some(
            (0..count)
                .map(|i| self.cell(bound(i), bound(i + 1), size))
                .collect(),
        )
    }

    /// The state of the bytes `lo..hi` of a blob of `size` bytes.
    fn cell(&self, lo: u64, hi: u64, size: u64) -> Cell {
        match self.phase {
            Phase::Verified => return Cell::Verified,
            Phase::IntegrityRejected => return Cell::Rejected,
            Phase::Active | Phase::Finalizing | Phase::Merged => {}
        }
        let overlaps = |offset: u64, len: Option<u64>| {
            let end = len.map_or(size, |len| offset.saturating_add(len));
            offset < hi && lo < end
        };
        if let Some(active) = self.active
            && overlaps(active.offset, active.len)
        {
            return match active.state {
                ChunkState::InFlight => Cell::InFlight,
                ChunkState::Retrying => Cell::Retrying,
                ChunkState::Rejected => Cell::Rejected,
            };
        }
        if hi <= self.held {
            return Cell::Held;
        }
        if hi > self.done {
            return Cell::Pending;
        }
        let mut cell = Cell::Acked;
        for scar in self.scars.iter().filter(|s| overlaps(s.offset, s.len)) {
            cell = match scar.kind {
                ScarKind::Rejected => return Cell::AckedAfterReject,
                ScarKind::Retried => Cell::AckedAfterRetry,
            };
        }
        cell
    }

    /// The server's offset is `to`: everything below it is held, nothing above it is.
    fn realign(&mut self, to: u64) {
        self.done = to;
        self.held = self.held.min(to);
    }

    /// The transfer now runs in `session_id` over a blob of `size` bytes; a session id not seen
    /// last counts as one more session.
    fn open_session(&mut self, session_id: String, size: u64) {
        if self.session_id.as_deref() != Some(session_id.as_str()) {
            self.sessions += 1;
        }
        self.session_id = Some(session_id);
        self.size = Some(size);
        self.active = None;
    }

    /// A resume reported `offset` as already held. Before this run moved a byte, that prefix
    /// is the server's from an earlier run.
    fn resume_from(&mut self, offset: u64) {
        if offset > 0 {
            self.resumes += 1;
            self.resumed_at = Some(offset);
        }
        if self.moved == 0 {
            self.held = offset;
        }
        self.realign(offset);
    }

    fn scar(&mut self, offset: u64, len: Option<u64>, kind: ScarKind) {
        if self.scars.len() == SCARS_KEPT {
            self.scars.remove(0);
        }
        self.scars.push(Scar { offset, len, kind });
    }

    fn sent(&mut self, offset: u64, len: u64) {
        self.active = Some(ActiveChunk {
            offset,
            len: Some(len),
            state: ChunkState::InFlight,
        });
        // The last chunk is whatever remains, not the adaptive size.
        let tail = self
            .size
            .is_some_and(|size| offset.saturating_add(len) >= size);
        if tail || self.chunk_size() == Some(len) {
            return;
        }
        if !self.chunk_sizes.is_empty() {
            self.chunk_size_changes += 1;
        }
        self.chunk_sizes.retain(|size| *size != len);
        if self.chunk_sizes.len() == CHUNK_SIZES_KEPT {
            self.chunk_sizes.remove(0);
        }
        self.chunk_sizes.push(len);
    }

    fn moved_bytes(&mut self, at_ms: u64, bytes: u64) {
        self.moved = self.moved.saturating_add(bytes);
        self.samples.push_back((at_ms, bytes));
    }

    fn prune(&mut self, now_ms: u64) {
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| !in_window(*at, now_ms))
        {
            self.samples.pop_front();
        }
    }
}

/// Whether a sample at `at_ms` is inside the throughput window ending at `now_ms`.
fn in_window(at_ms: u64, now_ms: u64) -> bool {
    now_ms
        .checked_sub(THROUGHPUT_WINDOW_MS)
        .is_none_or(|since| at_ms > since)
}

/// Custody answers seen, across every asset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Custody {
    /// Assets the server called durable.
    pub durable: u64,
    /// Assets with at least one blob not durable.
    pub not_durable: u64,
    /// Receipts verified.
    pub receipts: u64,
}

/// Every transfer the view has seen, folded from the event stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferBoard {
    now_ms: u64,
    transfers: Vec<Transfer>,
    missed: u64,
    custody: Custody,
}

impl TransferBoard {
    /// The board's clock: the latest stamp or tick seen.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// The transfers, oldest first.
    #[must_use]
    pub fn transfers(&self) -> &[Transfer] {
        &self.transfers
    }

    /// Events the view fell too far behind to receive.
    #[must_use]
    pub fn missed(&self) -> u64 {
        self.missed
    }

    /// Custody answers seen.
    #[must_use]
    pub fn custody(&self) -> Custody {
        self.custody
    }

    /// Bytes per second across every transfer over the window ending now.
    #[must_use]
    pub fn throughput(&self) -> u64 {
        self.transfers
            .iter()
            .map(|t| t.throughput(self.now_ms))
            .sum()
    }

    /// Transfers not yet finished.
    #[must_use]
    pub fn active(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| !t.phase.is_finished())
            .count()
    }

    /// Move the clock to `now_ms` (never backwards), so throughput decays while nothing moves.
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        let now = self.now_ms;
        for transfer in &mut self.transfers {
            transfer.prune(now);
        }
        self.drop_stale();
    }

    /// The subscriber fell behind and `count` events never arrived.
    pub fn lagged(&mut self, count: u64) {
        tracing::warn!(target: "capsule_cli::tui", count, "transfer view missed events");
        self.missed = self.missed.saturating_add(count);
    }

    /// Fold one event in.
    pub fn apply(&mut self, stamped: Stamped) {
        let Stamped { at_ms, event } = stamped;
        tracing::trace!(target: "capsule_cli::tui", at_ms, ?event, "transfer event");
        self.tick(at_ms);
        let at = self.now_ms;
        match event {
            TransferEvent::SessionCreated {
                blob,
                session_id,
                size,
                resume_offset,
            } => {
                // A create after a lost session continues the same transfer; after finalizing,
                // it is a new upload of the same blob.
                let t = self.entry(Direction::Upload, &blob, at, |p| p == Phase::Active);
                t.open_session(session_id, size);
                t.resume_from(resume_offset);
            }
            TransferEvent::SessionResumed {
                blob,
                session_id,
                size,
                offset,
            } => {
                let t = self.upload(&blob, at);
                t.open_session(session_id, size);
                t.resume_from(offset);
            }
            TransferEvent::ChunkSent {
                blob, offset, len, ..
            } => self.upload(&blob, at).sent(offset, len),
            TransferEvent::ChunkAcked {
                blob,
                offset,
                acked_offset,
                ..
            } => {
                let t = self.upload(&blob, at);
                t.moved_bytes(at, acked_offset.saturating_sub(offset));
                t.done = acked_offset;
                t.active = None;
            }
            TransferEvent::ChunkRetried {
                blob,
                offset,
                len,
                reason,
                ..
            } => {
                let t = self.upload(&blob, at);
                t.retries += 1;
                t.last_retry = Some(RetryNote::Upload(reason));
                t.scar(offset, Some(len), ScarKind::Retried);
                t.active = Some(ActiveChunk {
                    offset,
                    len: Some(len),
                    state: ChunkState::Retrying,
                });
                if let RetryReason::OffsetRealign { to } = reason {
                    t.realign(to);
                }
            }
            TransferEvent::ChecksumRejected {
                blob, offset, len, ..
            } => {
                let t = self.upload(&blob, at);
                t.rejects += 1;
                t.scar(offset, Some(len), ScarKind::Rejected);
                t.active = Some(ActiveChunk {
                    offset,
                    len: Some(len),
                    state: ChunkState::Rejected,
                });
            }
            TransferEvent::Finalize {
                blob,
                session_id,
                size,
            } => {
                let t = self.upload(&blob, at);
                t.session_id = Some(session_id);
                t.size = Some(size);
                t.done = size;
                t.active = None;
                t.phase = Phase::Finalizing;
            }
            TransferEvent::Merged { blob, .. } => {
                let t = self.upload(&blob, at);
                t.active = None;
                t.phase = Phase::Merged;
            }
            TransferEvent::Verify { durable, .. } => {
                if durable {
                    self.custody.durable += 1;
                } else {
                    self.custody.not_durable += 1;
                }
            }
            TransferEvent::Receipt {
                blob, receipt_seq, ..
            } => {
                self.custody.receipts += 1;
                // A receipt verifies the latest upload of the blob, whatever its phase; one for
                // a blob this view never saw uploaded is counted and nothing more.
                if let Some(t) = self
                    .transfers
                    .iter_mut()
                    .rev()
                    .find(|t| t.direction == Direction::Upload && t.blob == blob)
                {
                    t.last_ms = at;
                    t.receipt_seq = Some(receipt_seq);
                    if t.phase != Phase::Merged {
                        t.done = t.size.unwrap_or(t.done);
                        t.active = None;
                        t.phase = Phase::Verified;
                    }
                }
            }
            TransferEvent::RangeRequested {
                blob,
                offset,
                max_len,
            } => {
                let t = self.fetch(&blob, at);
                t.active = Some(ActiveChunk {
                    offset,
                    len: max_len,
                    state: ChunkState::InFlight,
                });
                // A fetch keeps one window for its whole run (only the tail asks for less), so
                // its first bounded window is its chunk size and it never changes.
                if let Some(len) = max_len
                    && t.chunk_sizes.is_empty()
                {
                    t.chunk_sizes.push(len);
                }
            }
            TransferEvent::RangeReceived { blob, offset, len } => {
                let t = self.fetch(&blob, at);
                t.moved_bytes(at, len);
                t.done = offset.saturating_add(len);
                t.active = None;
            }
            TransferEvent::FetchRetried {
                blob,
                offset,
                stalled,
                wait_ms,
            } => {
                let t = self.fetch(&blob, at);
                t.retries += 1;
                t.last_retry = Some(RetryNote::Fetch { stalled, wait_ms });
                let len = t.active.and_then(|a| a.len);
                t.scar(offset, len, ScarKind::Retried);
                t.active = Some(ActiveChunk {
                    offset,
                    len,
                    state: ChunkState::Retrying,
                });
                t.realign(offset);
            }
            TransferEvent::FetchVerified { blob, size } => {
                let t = self.fetch(&blob, at);
                t.size = Some(size);
                t.done = size;
                t.active = None;
                t.phase = Phase::Verified;
            }
            TransferEvent::IntegrityRejected { blob, size } => {
                let t = self.fetch(&blob, at);
                t.size = Some(size);
                t.rejects += 1;
                t.active = None;
                t.phase = Phase::IntegrityRejected;
            }
        }
        self.drop_old_finished();
        self.cap_total();
    }

    /// The live upload of `blob`, or a new one (the stream may have lagged past its create).
    fn upload(&mut self, blob: &str, at_ms: u64) -> &mut Transfer {
        self.entry(Direction::Upload, blob, at_ms, |p| !p.is_finished())
    }

    /// The live fetch of `blob`, or a new one.
    fn fetch(&mut self, blob: &str, at_ms: u64) -> &mut Transfer {
        self.entry(Direction::Fetch, blob, at_ms, |p| !p.is_finished())
    }

    /// The latest transfer of `blob` that way whose phase `continues`, or a new one, with its
    /// last-event time set to `at_ms`.
    fn entry(
        &mut self,
        direction: Direction,
        blob: &str,
        at_ms: u64,
        continues: impl Fn(Phase) -> bool,
    ) -> &mut Transfer {
        let found = self
            .transfers
            .iter()
            .rposition(|t| t.direction == direction && t.blob == blob && continues(t.phase));
        let index = found.unwrap_or_else(|| {
            tracing::debug!(target: "capsule_cli::tui", blob, ?direction, at_ms, "transfer view: new transfer");
            self.transfers
                .push(Transfer::new(blob, direction, at_ms));
            self.transfers.len() - 1
        });
        let transfer = &mut self.transfers[index];
        transfer.last_ms = at_ms;
        transfer
    }

    /// Drop unfinished transfers silent for [`STALE_AFTER_MS`] of stream time.
    fn drop_stale(&mut self) {
        let now = self.now_ms;
        let before = self.transfers.len();
        self.transfers
            .retain(|t| t.phase.is_finished() || now.saturating_sub(t.last_ms) < STALE_AFTER_MS);
        let dropped = before - self.transfers.len();
        if dropped > 0 {
            tracing::debug!(target: "capsule_cli::tui", dropped, now, "transfer view: aged out stale transfers");
        }
    }

    /// Hold at most [`TRANSFERS_KEPT`] transfers: drop the one silent longest until it fits.
    fn cap_total(&mut self) {
        while self.transfers.len() > TRANSFERS_KEPT {
            let Some(oldest) = self
                .transfers
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| t.last_ms)
                .map(|(i, _)| i)
            else {
                return;
            };
            let gone = self.transfers.remove(oldest);
            tracing::debug!(target: "capsule_cli::tui", blob = %gone.blob, "transfer view: over the cap, dropped");
        }
    }

    fn drop_old_finished(&mut self) {
        let finished = self
            .transfers
            .iter()
            .filter(|t| t.phase.is_finished())
            .count();
        let mut excess = finished.saturating_sub(FINISHED_KEPT);
        if excess > 0 {
            self.transfers.retain(|t| {
                if excess > 0 && t.phase.is_finished() {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
        }
    }
}

#[cfg(test)]
mod tests;
