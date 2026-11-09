//! The device-wide upload queue, keyed per server (transfer upload doc, §Queueing Across
//! Servers).
//!
//! A client signed in to more than one server drives one **lane** per server. Each lane owns
//! its own work and its own pacing: the jobs still to send, its two **in-flight windows**, its
//! shared **retry budget**, and the time before which it must not be served (a backoff, or a
//! server's `Retry-After`).
//!
//! - **W, the sessions window**: how many of the lane's sessions may be open at once. Several
//!   sessions in flight, each on its own HTTP/1.1 connection, are what keep a high-latency or
//!   lossy link busy, because one connection's throughput is bounded by its round trip.
//! - **P, the ranges window**: how many ranges each session may hold in flight. A session's P is
//!   fixed when it is granted and can only be narrowed afterwards, by the cap the server
//!   advertised for that session ([`UploadQueue::set_session_ranges`]).
//!
//! Both are client-local caps ([`LaneWindows`], at most the compiled [`MAX_SESSIONS_WINDOW`] and
//! [`MAX_RANGES_WINDOW`]) that the server's `X-Capsule-Advice` can only lower
//! ([`UploadQueue::set_advice`]), and W never exceeds the account's live-session cap
//! ([`UploadQueue::set_session_cap`]). They are not the protocol window, which says which
//! protocol dates a server accepts and bounds nothing in flight.
//!
//! What the lanes share is one bounded **byte budget**. A session admitted to run reserves
//! nothing; every *range* it sends reserves its length while in flight
//! ([`UploadQueue::reserve_range`]), so the device never buffers more than the budget however
//! many servers and sessions it drives, and a large blob costs no more than the windows it is
//! sending. Ranges waiting for the budget are served first-come, so a lane sending originals
//! cannot keep re-reserving it ahead of another lane's T0 range.
//!
//! [`UploadQueue::next`] serves the lanes **round-robin**, so one server's backlog cannot
//! starve another's, and inside a lane it always takes the lowest [`UploadTier`] first, so the
//! staged ladder (T0 → T1 → T2) holds per server. A lane whose transfers report no progress
//! for the stall bound gives its sessions and their range reservations back
//! ([`UploadQueue::reclaim_stalled`]); its jobs return to the front of their tiers and the lane
//! waits out one stall bound before it is served again, so a dead server's lane cannot hold the
//! budget every other lane needs. A job that failed and may run again spends one retry from its
//! lane's shared budget ([`UploadQueue::retry`]), so a server that fails everything costs a
//! bounded number of retries however many jobs are queued for it. On resume each lane is
//! rebuilt from its own server's truth ([`UploadQueue::rebuild_lane`]); no lane's state is
//! derived from another's. Deleting an asset withdraws its jobs from every lane at once
//! ([`UploadQueue::withdraw`]), so no further byte of it is sent to any server.
//!
//! This is a pure scheduler over values: it sends nothing and reads no clock. The caller
//! passes `now`, runs each [`Grant`] it is handed, and reports back with
//! [`UploadQueue::progress`], [`UploadQueue::complete`], [`UploadQueue::requeue`] or
//! [`UploadQueue::retry`]. With one lane and a sessions window of one it hands out exactly one
//! job at a time, in tier order - the single-server sequential push.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use capsule_library::import::UploadTier;

use super::advice::{MAX_ADVISED_RANGES, MAX_ADVISED_SESSIONS, ServerAdvice};
use crate::net::{DEFAULT_LANE_RETRY_BUDGET, LaneRetries};

/// The widest sessions window (W) any lane holds: the most advice can ever grant.
pub const MAX_SESSIONS_WINDOW: u32 = MAX_ADVISED_SESSIONS;
/// The widest ranges window (P) any session holds: the protocol's ceiling on a session's
/// in-flight cap.
pub const MAX_RANGES_WINDOW: u32 = MAX_ADVISED_RANGES;
/// The sessions a lane holds open at once when nothing narrower applies. Four connections keep
/// a 600 ms, lossy path busy without opening more sessions than a small server's live-session
/// cap admits.
pub const DEFAULT_SESSIONS_WINDOW: u32 = 4;
/// The ranges a session holds in flight when nothing narrower applies.
pub const DEFAULT_RANGES_WINDOW: u32 = 4;

/// A lane's own in-flight caps, before the server's advice lowers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneWindows {
    /// W: sessions open at once on the lane.
    pub sessions: u32,
    /// P: ranges in flight per session.
    pub ranges: u32,
}

impl Default for LaneWindows {
    fn default() -> Self {
        Self::new(DEFAULT_SESSIONS_WINDOW, DEFAULT_RANGES_WINDOW)
    }
}

impl LaneWindows {
    /// Caps of `sessions` and `ranges`, each clamped into `1..=` its compiled maximum.
    #[must_use]
    pub fn new(sessions: u32, ranges: u32) -> Self {
        Self {
            sessions: sessions.clamp(1, MAX_SESSIONS_WINDOW),
            ranges: ranges.clamp(1, MAX_RANGES_WINDOW),
        }
    }
}

/// The key a lane is held under: the server's canonical origin (`scheme://host[:port]`), so
/// one server reached through differently written endpoints shares one lane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerKey(String);

impl ServerKey {
    /// Key a lane under `origin`, which the caller has already canonicalized.
    pub fn new(origin: impl Into<String>) -> Self {
        Self(origin.into())
    }

    /// The origin this key was built from.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One unit of upload work queued on a lane: a session to drive, at its tier. Queuing or
/// running it reserves none of the byte budget; only the ranges it sends do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedJob<J> {
    /// The staged tier the job belongs to; a lane always serves its lowest tier first.
    pub tier: UploadTier,
    /// What the caller needs to run it.
    pub job: J,
}

impl<J> QueuedJob<J> {
    /// A job at `tier`.
    pub fn new(tier: UploadTier, job: J) -> Self {
        Self { tier, job }
    }
}

/// The handle of one job in flight, unique for the life of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrantId(u64);

/// The handle of one range reservation, unique for the life of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RangeId(u64);

/// A job the queue admitted: run it, then report on it by its [`GrantId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant<J> {
    /// The handle every later report names.
    pub id: GrantId,
    /// The lane (server) the job belongs to.
    pub server: ServerKey,
    /// The job's tier.
    pub tier: UploadTier,
    /// P: the most ranges this session may hold in flight, fixed at the grant. The server's
    /// own cap for the session can only narrow it ([`UploadQueue::set_session_ranges`]).
    pub ranges: u32,
    /// The work itself.
    pub job: J,
}

/// Why [`UploadQueue::reserve_range`] reserved nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRefused {
    /// The grant is no longer in flight (completed, requeued, revoked or superseded): stop
    /// sending for it.
    NotInFlight,
    /// The session already holds its P ranges in flight: wait for one to return.
    WindowFull,
    /// The shared byte budget cannot hold the range now, or an earlier grant waits for it
    /// first: wait for a reservation to be released and ask again.
    BudgetFull,
}

/// What [`UploadQueue::retry`] decided for a job that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryAdmission<J> {
    /// The lane's budget paid for a retry: the job is back at the front of its tier.
    Requeued,
    /// The lane's retry budget is spent: the job leaves the queue, and the caller surfaces it as
    /// the failure it is.
    BudgetSpent(QueuedJob<J>),
    /// The grant was no longer in flight; nothing changed.
    NotInFlight,
}

/// A job in flight, kept so a revoked or requeued grant can return to its lane.
#[derive(Debug, Clone)]
struct InFlight<J> {
    id: GrantId,
    job: QueuedJob<J>,
    /// P for this session.
    ranges_window: u32,
    /// The session's ranges in flight and the bytes each holds of the budget.
    ranges: Vec<(RangeId, u64)>,
}

impl<J> InFlight<J> {
    fn reserved(&self) -> u64 {
        self.ranges.iter().map(|(_, bytes)| bytes).sum()
    }
}

/// One server's lane.
#[derive(Debug)]
struct Lane<J> {
    server: ServerKey,
    /// The client's own caps for this lane.
    windows: LaneWindows,
    /// The last advice the server sent for this lane, and when it arrived.
    advice: Option<(ServerAdvice, Instant)>,
    /// The account's live-session cap on this server, when known.
    session_cap: Option<u32>,
    /// The lane's shared retry budget; the client driving the lane holds a clone.
    retries: LaneRetries,
    /// Queued jobs, one FIFO per tier, indexed in ladder order.
    queued: [VecDeque<QueuedJob<J>>; 3],
    in_flight: Vec<InFlight<J>>,
    /// When this lane last made progress (a grant, a progress report, or a completion).
    last_progress: Option<Instant>,
    /// The lane is not served before this instant: a backoff, `Retry-After`, or a stall.
    paused_until: Option<Instant>,
}

/// Where `tier` sits in a lane's per-tier queues.
fn tier_slot(tier: UploadTier) -> usize {
    match tier {
        UploadTier::Index => 0,
        UploadTier::Preview => 1,
        UploadTier::Original => 2,
    }
}

impl<J> Lane<J> {
    fn new(server: ServerKey, windows: LaneWindows, retry_budget: u32) -> Self {
        Self {
            server,
            windows,
            advice: None,
            session_cap: None,
            retries: LaneRetries::new(retry_budget),
            queued: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            in_flight: Vec::new(),
            last_progress: None,
            paused_until: None,
        }
    }

    /// W at `now`: the lane's own cap, lowered by advice still within its `ttl` and by the
    /// account's live-session cap. Never below one.
    fn sessions_window(&self, now: Instant) -> u32 {
        let own = self.windows.sessions;
        let advised = self.advice.map_or(own, |(advice, received)| {
            advice.sessions_window(own, now.saturating_duration_since(received))
        });
        let capped = self.session_cap.map_or(advised, |cap| advised.min(cap));
        capped.max(1)
    }

    /// P for a session granted now: the lane's own cap, lowered by the last `ranges` the server
    /// advised. A session's own advice narrows it further once its create answers.
    fn ranges_window(&self) -> u32 {
        let own = self.windows.ranges;
        self.advice
            .map_or(own, |(advice, _)| advice.ranges_window(own))
    }

    /// Take the job this lane serves next: the front of its lowest non-empty tier.
    fn pop_head(&mut self) -> Option<QueuedJob<J>> {
        self.queued.iter_mut().find_map(VecDeque::pop_front)
    }

    fn queued_len(&self) -> usize {
        self.queued.iter().map(VecDeque::len).sum()
    }

    fn paused_at(&self, now: Instant) -> bool {
        self.paused_until.is_some_and(|until| now < until)
    }
}

/// The per-server upload queue with one shared byte budget. See the module docs.
#[derive(Debug)]
pub struct UploadQueue<J> {
    budget: u64,
    in_use: u64,
    stall_after: Duration,
    /// The caps a newly opened lane starts with.
    windows: LaneWindows,
    /// The retry budget a newly opened lane starts with.
    retry_budget: u32,
    /// Lanes in the order they were opened; round-robin walks this order.
    lanes: Vec<Lane<J>>,
    /// The lane the next round-robin pass starts from.
    cursor: usize,
    /// Grants whose range the budget refused, in the order they first asked. The budget serves
    /// them first-come: no range is reserved past the head of this line.
    waiting: VecDeque<GrantId>,
    next_grant: u64,
    next_range: u64,
}

impl<J: Clone> UploadQueue<J> {
    /// An empty queue whose lanes share `budget` bytes in flight (at least one byte), and give
    /// their reservations up after `stall_after` without progress. Lanes open with the default
    /// [`LaneWindows`] and a [`DEFAULT_LANE_RETRY_BUDGET`].
    pub fn new(budget: u64, stall_after: Duration) -> Self {
        Self {
            budget: budget.max(1),
            in_use: 0,
            stall_after,
            windows: LaneWindows::default(),
            retry_budget: DEFAULT_LANE_RETRY_BUDGET,
            lanes: Vec::new(),
            cursor: 0,
            waiting: VecDeque::new(),
            next_grant: 0,
            next_range: 0,
        }
    }

    /// Open every lane after this with `windows` as its own caps. A lane already open keeps its
    /// caps; [`Self::set_windows`] changes one.
    #[must_use]
    pub fn with_windows(mut self, windows: LaneWindows) -> Self {
        self.windows = windows;
        self
    }

    /// Open every lane after this with a retry budget of `retries`.
    #[must_use]
    pub fn with_retry_budget(mut self, retries: u32) -> Self {
        self.retry_budget = retries;
        self
    }

    /// The shared byte budget.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// The bytes reserved by ranges in flight, across every lane.
    pub fn in_use(&self) -> u64 {
        self.in_use
    }

    /// `server`'s shared retry budget, opening its lane if it has none. Hand it to the client
    /// that drives the lane's sessions ([`UploadClient::with_lane_retries`]) so the client's
    /// request retries and this queue's [`Self::retry`] spend from one pool.
    ///
    /// [`UploadClient::with_lane_retries`]: crate::upload::UploadClient::with_lane_retries
    pub fn lane_retries(&mut self, server: &ServerKey) -> LaneRetries {
        self.lane_mut_or_open(server).retries.clone()
    }

    /// W for `server` at `now`: the lane's own cap lowered by fresh advice and the live-session
    /// cap; the default cap for a server with no lane.
    pub fn sessions_window(&self, server: &ServerKey, now: Instant) -> u32 {
        self.lane(server)
            .map_or(self.windows.sessions, |lane| lane.sessions_window(now))
    }

    /// The servers that hold a lane, in round-robin order.
    pub fn servers(&self) -> impl Iterator<Item = &ServerKey> {
        self.lanes.iter().map(|lane| &lane.server)
    }

    /// Jobs still queued (not in flight) on `server`'s lane; zero for a server with none.
    pub fn queued(&self, server: &ServerKey) -> usize {
        self.lane(server).map_or(0, Lane::queued_len)
    }

    /// Jobs in flight on `server`'s lane.
    pub fn in_flight(&self, server: &ServerKey) -> usize {
        self.lane(server).map_or(0, |lane| lane.in_flight.len())
    }

    /// Whether no lane holds any work, queued or in flight.
    pub fn is_empty(&self) -> bool {
        self.lanes
            .iter()
            .all(|lane| lane.queued_len() == 0 && lane.in_flight.is_empty())
    }

    /// Set `server`'s own caps (power saver or a thermal state lowers them), opening its lane if
    /// it has none. A narrower window takes effect as work finishes; nothing in flight is
    /// revoked, and a session already granted keeps its P.
    pub fn set_windows(&mut self, server: &ServerKey, windows: LaneWindows) {
        let lane = self.lane_mut_or_open(server);
        lane.windows = LaneWindows::new(windows.sessions, windows.ranges);
        tracing::debug!(
            server = %server,
            sessions = lane.windows.sessions,
            ranges = lane.windows.ranges,
            "upload queue: lane windows set"
        );
    }

    /// Record the `X-Capsule-Advice` `server` sent at `received`, replacing any earlier advice.
    /// Its `sessions` lowers W until its `ttl` runs out; its `ranges` lowers P for the sessions
    /// granted after it. Advice can only lower the lane's own caps, never raise them.
    pub fn set_advice(&mut self, server: &ServerKey, advice: ServerAdvice, received: Instant) {
        let lane = self.lane_mut_or_open(server);
        lane.advice = Some((advice, received));
        tracing::debug!(
            server = %server,
            sessions = lane.sessions_window(received),
            ranges = lane.ranges_window(),
            ttl_s = advice.ttl().as_secs(),
            "upload queue: lane advice applied"
        );
    }

    /// Cap W at the account's live-session cap on `server` (at least one), so the lane never
    /// opens a session the server would refuse for being one too many.
    pub fn set_session_cap(&mut self, server: &ServerKey, cap: u32) {
        let lane = self.lane_mut_or_open(server);
        lane.session_cap = Some(cap.max(1));
        tracing::debug!(server = %server, cap, "upload queue: live-session cap set");
    }

    /// Narrow grant `id`'s P to the in-flight cap its server advertised for that session (the
    /// `ranges` of the advice on its create or `HEAD`). A session's P never widens. Returns the
    /// session's P, or `None` when the grant is no longer in flight.
    pub fn set_session_ranges(&mut self, id: GrantId, advice: &ServerAdvice) -> Option<u32> {
        let (lane, at) = self.locate(id)?;
        let flight = &mut self.lanes[lane].in_flight[at];
        flight.ranges_window = advice.ranges_window(flight.ranges_window);
        tracing::debug!(
            grant = id.0,
            ranges = flight.ranges_window,
            "upload queue: session ranges window set"
        );
        Some(flight.ranges_window)
    }

    /// Queue `job` at the back of its tier on `server`'s lane, opening the lane if it has none.
    pub fn enqueue(&mut self, server: &ServerKey, job: QueuedJob<J>) {
        let lane = self.lane_mut_or_open(server);
        tracing::trace!(server = %server, tier = ?job.tier, "upload queue: job queued");
        lane.queued[tier_slot(job.tier)].push_back(job);
    }

    /// Replace everything `server`'s lane holds with `jobs`, derived from that server's own
    /// truth on resume. Any job still in flight on the lane is superseded and its range
    /// reservations released (the caller has stopped it); its id is returned and later reports
    /// on it are ignored. The lane's windows, advice and retry budget are kept, and so is any
    /// pause still running (a server's `Retry-After` binds whatever work the lane holds).
    pub fn rebuild_lane(
        &mut self,
        server: &ServerKey,
        jobs: impl IntoIterator<Item = QueuedJob<J>>,
    ) -> Vec<GrantId> {
        let lane = self.lane_mut_or_open(server);
        let dropped_queued = lane.queued_len();
        for tier in &mut lane.queued {
            tier.clear();
        }
        let superseded: Vec<InFlight<J>> = lane.in_flight.drain(..).collect();
        lane.last_progress = None;
        for job in jobs {
            lane.queued[tier_slot(job.tier)].push_back(job);
        }
        let queued = lane.queued_len();
        let released: u64 = superseded.iter().map(InFlight::reserved).sum();
        self.in_use -= released;
        tracing::info!(
            server = %server,
            dropped_queued,
            superseded = superseded.len(),
            released,
            queued,
            "upload queue: lane rebuilt from server truth"
        );
        superseded.into_iter().map(|grant| grant.id).collect()
    }

    /// Remove every job `selects` picks from every lane, queued or in flight: the redaction a
    /// delete makes of one asset's upload (Organization — Deletion State Machine), which spans
    /// every server the asset was queued for. A job in flight is superseded and its range
    /// reservations released (the caller stops it); its id is returned and later reports on it
    /// are ignored. Every other job keeps its place, and no lane's windows or pause change.
    pub fn withdraw(&mut self, selects: impl Fn(&J) -> bool) -> Vec<GrantId> {
        let mut superseded = Vec::new();
        let mut released = 0;
        for lane in &mut self.lanes {
            let mut dropped = 0;
            for tier in &mut lane.queued {
                let before = tier.len();
                tier.retain(|queued| !selects(&queued.job));
                dropped += before - tier.len();
            }
            let (stopped, kept): (Vec<InFlight<J>>, Vec<InFlight<J>>) = lane
                .in_flight
                .drain(..)
                .partition(|flight| selects(&flight.job.job));
            lane.in_flight = kept;
            let lane_released: u64 = stopped.iter().map(InFlight::reserved).sum();
            released += lane_released;
            if dropped > 0 || !stopped.is_empty() {
                tracing::info!(
                    server = %lane.server,
                    dropped_queued = dropped,
                    superseded = stopped.len(),
                    released = lane_released,
                    "upload queue: jobs withdrawn"
                );
            }
            superseded.extend(stopped.into_iter().map(|flight| flight.id));
        }
        self.in_use -= released;
        superseded.sort_unstable();
        superseded
    }

    /// Hold `server`'s lane until `until` (its backoff, or the server's `Retry-After`). Other
    /// lanes are served meanwhile. A later deadline replaces an earlier one.
    pub fn defer(&mut self, server: &ServerKey, until: Instant) {
        let lane = self.lane_mut_or_open(server);
        lane.paused_until = Some(lane.paused_until.map_or(until, |held| held.max(until)));
        tracing::debug!(server = %server, "upload queue: lane deferred");
    }

    /// Admit the next job, if any lane can run one now. Admission reserves no bytes; the
    /// session's ranges do, through [`Self::reserve_range`].
    ///
    /// Lanes are tried round-robin from the one after the lane served last. A lane is passed
    /// over while it is paused, has no queued job, or already holds its W sessions in flight; a
    /// lane never skips past its own head, so tier order holds inside it.
    pub fn next(&mut self, now: Instant) -> Option<Grant<J>> {
        let count = self.lanes.len();
        for step in 0..count {
            let index = (self.cursor + step) % count;
            let lane = &mut self.lanes[index];
            if lane.paused_at(now) || lane.in_flight.len() >= lane.sessions_window(now) as usize {
                continue;
            }
            let Some(job) = lane.pop_head() else { continue };
            let id = GrantId(self.next_grant);
            self.next_grant += 1;
            let ranges = lane.ranges_window();
            lane.paused_until = None;
            lane.last_progress = Some(now);
            lane.in_flight.push(InFlight {
                id,
                job: job.clone(),
                ranges_window: ranges,
                ranges: Vec::new(),
            });
            let server = lane.server.clone();
            let in_flight = lane.in_flight.len();
            self.cursor = (index + 1) % count;
            tracing::debug!(
                server = %server,
                grant = id.0,
                tier = ?job.tier,
                ranges,
                in_flight,
                "upload queue: job admitted"
            );
            return Some(Grant {
                id,
                server,
                tier: job.tier,
                ranges,
                job: job.job,
            });
        }
        None
    }

    /// Reserve `len` bytes of the shared budget for one range grant `id` is about to send.
    ///
    /// The reservation is the range's length clamped to the budget, so a range longer than the
    /// whole budget still runs once the budget is otherwise free. It is refused while the
    /// session holds its P ranges in flight ([`RangeRefused::WindowFull`]) or the budget left
    /// cannot hold it ([`RangeRefused::BudgetFull`]); the caller waits for a release and asks
    /// again. A reservation counts as progress on the lane.
    ///
    /// The budget serves waiting grants **first-come**: a grant refused for the budget joins a
    /// line in the order it first asked, and while that line holds an earlier grant every later
    /// ask is refused too, however little it wants. So a holder that releases its range and
    /// asks for the next one in the same breath goes behind whoever was already waiting, and a
    /// range clamped to the whole budget runs once the reservations ahead of it drain, rather
    /// than waiting for every holder to stop on its own. A grant that leaves flight leaves the
    /// line with it; a caller that stops asking for a grant it still holds must complete,
    /// requeue or retry it, or the line waits on it.
    pub fn reserve_range(
        &mut self,
        id: GrantId,
        len: u64,
        now: Instant,
    ) -> Result<RangeId, RangeRefused> {
        let Some((lane_at, at)) = self.locate(id) else {
            return Err(RangeRefused::NotInFlight);
        };
        let reserved = len.min(self.budget);
        let free = self.budget - self.in_use;
        {
            let flight = &self.lanes[lane_at].in_flight[at];
            if flight.ranges.len() >= flight.ranges_window as usize {
                tracing::trace!(
                    grant = id.0,
                    ranges = flight.ranges_window,
                    "upload queue: session ranges window full"
                );
                return Err(RangeRefused::WindowFull);
            }
        }
        self.prune_waiting();
        let ahead = self
            .waiting
            .iter()
            .take_while(|waiter| **waiter != id)
            .count();
        if ahead > 0 || reserved > free {
            if !self.waiting.contains(&id) {
                self.waiting.push_back(id);
            }
            tracing::trace!(
                grant = id.0,
                reserved,
                free,
                ahead,
                "upload queue: range waits for the budget"
            );
            return Err(RangeRefused::BudgetFull);
        }
        if self.waiting.front() == Some(&id) {
            self.waiting.pop_front();
        }
        let lane = &mut self.lanes[lane_at];
        let flight = &mut lane.in_flight[at];
        let range = RangeId(self.next_range);
        self.next_range += 1;
        flight.ranges.push((range, reserved));
        lane.last_progress = Some(now);
        self.in_use += reserved;
        tracing::trace!(
            server = %lane.server,
            grant = id.0,
            range = range.0,
            reserved,
            in_use = self.in_use,
            "upload queue: range reserved"
        );
        Ok(range)
    }

    /// Range `range` returned (sent, refused or abandoned): release its bytes. It counts as
    /// progress on its lane. Returns whether the reservation was still held; one released with
    /// its session (a completion, requeue, revocation or rebuild) is already gone.
    pub fn release_range(&mut self, range: RangeId, now: Instant) -> bool {
        for lane in &mut self.lanes {
            for flight in &mut lane.in_flight {
                if let Some(at) = flight.ranges.iter().position(|(id, _)| *id == range) {
                    let (_, reserved) = flight.ranges.remove(at);
                    lane.last_progress = Some(now);
                    self.in_use -= reserved;
                    tracing::trace!(
                        range = range.0,
                        released = reserved,
                        in_use = self.in_use,
                        "upload queue: range released"
                    );
                    return true;
                }
            }
        }
        false
    }

    /// Grants still in flight that wait in the budget's line for a range, first-come. A caller
    /// that wakes its waiters on a release wakes them again after a reservation while this is
    /// not zero, so the next in line asks once the one ahead of it has been served.
    pub fn waiting_for_budget(&self) -> usize {
        self.waiting
            .iter()
            .filter(|waiter| self.locate(**waiter).is_some())
            .count()
    }

    /// Ranges grant `id` holds in flight; zero for a grant no longer in flight.
    pub fn ranges_in_flight(&self, id: GrantId) -> usize {
        self.locate(id)
            .map_or(0, |(lane, at)| self.lanes[lane].in_flight[at].ranges.len())
    }

    /// Record that grant `id` moved bytes, so its lane is not counted as stalled. Returns
    /// whether the grant is still in flight.
    pub fn progress(&mut self, id: GrantId, now: Instant) -> bool {
        match self.locate(id) {
            Some((lane, _)) => {
                self.lanes[lane].last_progress = Some(now);
                true
            }
            None => false,
        }
    }

    /// Grant `id` finished (sent, merged, or failed for good): release every range it still
    /// holds. Returns whether the grant was still in flight. The successes a lane's retry budget
    /// earns back from are the requests that succeeded, which the client driving the lane
    /// records on the shared budget ([`Self::lane_retries`]).
    pub fn complete(&mut self, id: GrantId, now: Instant) -> bool {
        let Some((lane, flight)) = self.take(id) else {
            return false;
        };
        let lane = &mut self.lanes[lane];
        lane.last_progress = Some(now);
        tracing::debug!(
            server = %lane.server,
            grant = id.0,
            released = flight.reserved(),
            in_use = self.in_use,
            "upload queue: job complete"
        );
        true
    }

    /// Grant `id` must run again because it was interrupted, not because it failed (a stall cut
    /// or a cancelled task): release its ranges and return its job to the front of its tier.
    /// Charges no retry. Returns whether the grant was still in flight.
    pub fn requeue(&mut self, id: GrantId) -> bool {
        let Some((lane, flight)) = self.take(id) else {
            return false;
        };
        let lane = &mut self.lanes[lane];
        tracing::debug!(server = %lane.server, grant = id.0, "upload queue: job requeued");
        lane.queued[tier_slot(flight.job.tier)].push_front(flight.job);
        true
    }

    /// Grant `id` failed with an error the classification permits retrying: release its ranges
    /// and spend one retry from its lane's shared budget. While the budget holds, the job
    /// returns to the front of its tier; once it is spent, the job leaves the queue and is
    /// handed back, so a server that fails everything costs at most the lane's budget.
    pub fn retry(&mut self, id: GrantId) -> RetryAdmission<J> {
        let Some((lane, flight)) = self.take(id) else {
            return RetryAdmission::NotInFlight;
        };
        let lane = &mut self.lanes[lane];
        if lane.retries.try_spend() {
            tracing::debug!(
                server = %lane.server,
                grant = id.0,
                retries_left = lane.retries.remaining(),
                "upload queue: job retried"
            );
            lane.queued[tier_slot(flight.job.tier)].push_front(flight.job);
            RetryAdmission::Requeued
        } else {
            tracing::warn!(
                server = %lane.server,
                grant = id.0,
                "upload queue: the lane's retry budget is spent; the job fails"
            );
            RetryAdmission::BudgetSpent(flight.job)
        }
    }

    /// Take the budget back from every lane whose jobs in flight have made no progress for the
    /// stall bound: their sessions and every range reservation they hold. Each such lane's jobs
    /// return to the front of their tiers, charging no retry, and the lane is
    /// not served for at least one more stall bound (longer where a later deadline is already
    /// held), so the budget goes to the lanes still moving.
    /// Returns the revoked grants, which the caller stops.
    pub fn reclaim_stalled(&mut self, now: Instant) -> Vec<GrantId> {
        let stall_after = self.stall_after;
        let mut revoked = Vec::new();
        let mut released = 0;
        for lane in &mut self.lanes {
            if lane.in_flight.is_empty() {
                continue;
            }
            let stalled = lane
                .last_progress
                .is_none_or(|last| now.saturating_duration_since(last) >= stall_after);
            if !stalled {
                continue;
            }
            let flights: Vec<InFlight<J>> = lane.in_flight.drain(..).collect();
            // Requeue in reverse so the earliest-granted job ends up at the front again.
            for flight in flights.into_iter().rev() {
                released += flight.reserved();
                revoked.push(flight.id);
                lane.queued[tier_slot(flight.job.tier)].push_front(flight.job);
            }
            // Keep a later deadline (a server's `Retry-After`) rather than shortening it.
            let hold_off = now + stall_after;
            lane.paused_until = Some(
                lane.paused_until
                    .map_or(hold_off, |held| held.max(hold_off)),
            );
            tracing::info!(
                server = %lane.server,
                stall_after_ms = stall_after.as_millis(),
                "upload queue: lane made no progress; its budget is reclaimed"
            );
        }
        self.in_use -= released;
        revoked.sort_unstable();
        revoked
    }

    fn lane(&self, server: &ServerKey) -> Option<&Lane<J>> {
        self.lanes.iter().find(|lane| &lane.server == server)
    }

    fn lane_mut_or_open(&mut self, server: &ServerKey) -> &mut Lane<J> {
        let index = if let Some(index) = self.lanes.iter().position(|lane| &lane.server == server) {
            index
        } else {
            tracing::info!(server = %server, "upload queue: lane opened");
            self.lanes
                .push(Lane::new(server.clone(), self.windows, self.retry_budget));
            self.lanes.len() - 1
        };
        &mut self.lanes[index]
    }

    /// The lane index and in-flight position of grant `id`.
    fn locate(&self, id: GrantId) -> Option<(usize, usize)> {
        self.lanes.iter().enumerate().find_map(|(lane, l)| {
            l.in_flight
                .iter()
                .position(|flight| flight.id == id)
                .map(|at| (lane, at))
        })
    }

    /// Drop the grants that left flight from the budget's waiting line.
    fn prune_waiting(&mut self) {
        let mut waiting = std::mem::take(&mut self.waiting);
        waiting.retain(|waiter| self.locate(*waiter).is_some());
        self.waiting = waiting;
    }

    /// Remove grant `id` from flight and release every range it holds.
    fn take(&mut self, id: GrantId) -> Option<(usize, InFlight<J>)> {
        let (lane, at) = self.locate(id)?;
        // `remove`, not `swap_remove`: flights stay in grant order for `reclaim_stalled`.
        let flight = self.lanes[lane].in_flight.remove(at);
        self.in_use -= flight.reserved();
        Some((lane, flight))
    }
}

#[cfg(test)]
mod tests;
