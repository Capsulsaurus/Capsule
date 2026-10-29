//! The device-wide upload queue, keyed per server (transfer upload doc, §Queueing Across
//! Servers).
//!
//! A client signed in to more than one server drives one **lane** per server. Each lane owns
//! its own work and its own pacing: the jobs still to send, the protocol window (how many of
//! its transfers may be in flight at once), and the time before which it must not be served
//! (a backoff, or a server's `Retry-After`). What the lanes share is one bounded **byte
//! budget**: every job in flight holds a reservation of its size against it, so the device
//! never buffers more than the budget however many servers it talks to.
//!
//! [`UploadQueue::next`] serves the lanes **round-robin**, so one server's backlog cannot
//! starve another's, and inside a lane it always takes the lowest [`UploadTier`] first, so the
//! staged ladder (T0 → T1 → T2) holds per server. A lane whose transfers report no progress
//! for the stall bound gives its reservations back ([`UploadQueue::reclaim_stalled`]); its
//! jobs return to the front of their tiers and the lane waits out one stall bound before it
//! is served again, so a dead server's lane cannot hold the budget every other lane needs.
//! On resume each lane is rebuilt from its own server's truth ([`UploadQueue::rebuild_lane`]);
//! no lane's state is derived from another's.
//!
//! This is a pure scheduler over values: it sends nothing and reads no clock. The caller
//! passes `now`, runs each [`Grant`] it is handed, and reports back with
//! [`UploadQueue::progress`], [`UploadQueue::complete`] or [`UploadQueue::requeue`]. With one
//! lane and a window of one it hands out exactly one job at a time, in tier order - the
//! single-server push unchanged.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use capsule_library::import::UploadTier;

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

/// One unit of upload work queued on a lane: a session to drive, at its tier, holding `bytes`
/// of the shared budget while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedJob<J> {
    /// The staged tier the job belongs to; a lane always serves its lowest tier first.
    pub tier: UploadTier,
    /// The bytes the job buffers while in flight (its blob's size).
    pub bytes: u64,
    /// What the caller needs to run it.
    pub job: J,
}

impl<J> QueuedJob<J> {
    /// A job at `tier` that holds `bytes` of the budget while in flight.
    pub fn new(tier: UploadTier, bytes: u64, job: J) -> Self {
        Self { tier, bytes, job }
    }
}

/// The handle of one job in flight, unique for the life of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrantId(u64);

/// A job the queue admitted: run it, then report on it by its [`GrantId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant<J> {
    /// The handle every later report names.
    pub id: GrantId,
    /// The lane (server) the job belongs to.
    pub server: ServerKey,
    /// The job's tier.
    pub tier: UploadTier,
    /// The bytes reserved against the shared budget: the job's size, clamped to the budget so
    /// a job larger than the whole budget still runs once the budget is otherwise free.
    pub reserved: u64,
    /// The work itself.
    pub job: J,
}

/// A job in flight, kept so a revoked or requeued grant can return to its lane.
#[derive(Debug, Clone)]
struct InFlight<J> {
    id: GrantId,
    reserved: u64,
    job: QueuedJob<J>,
}

/// One server's lane.
#[derive(Debug)]
struct Lane<J> {
    server: ServerKey,
    /// The most jobs this lane may hold in flight at once; at least one.
    window: usize,
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
    fn new(server: ServerKey, window: usize) -> Self {
        Self {
            server,
            window: window.max(1),
            queued: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            in_flight: Vec::new(),
            last_progress: None,
            paused_until: None,
        }
    }

    /// The job this lane serves next: the front of its lowest non-empty tier.
    fn head(&self) -> Option<&QueuedJob<J>> {
        self.queued.iter().find_map(VecDeque::front)
    }

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
    /// Lanes in the order they were opened; round-robin walks this order.
    lanes: Vec<Lane<J>>,
    /// The lane the next round-robin pass starts from.
    cursor: usize,
    next_grant: u64,
}

impl<J: Clone> UploadQueue<J> {
    /// An empty queue whose lanes share `budget` bytes in flight (at least one byte), and give
    /// their reservations up after `stall_after` without progress.
    pub fn new(budget: u64, stall_after: Duration) -> Self {
        Self {
            budget: budget.max(1),
            in_use: 0,
            stall_after,
            lanes: Vec::new(),
            cursor: 0,
            next_grant: 0,
        }
    }

    /// The shared byte budget.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// The bytes reserved by jobs in flight, across every lane.
    pub fn in_use(&self) -> u64 {
        self.in_use
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

    /// Set `server`'s protocol window (at least one), opening its lane if it has none. A
    /// narrower window takes effect as jobs finish; nothing in flight is revoked.
    pub fn set_window(&mut self, server: &ServerKey, window: usize) {
        let lane = self.lane_mut_or_open(server);
        lane.window = window.max(1);
        tracing::debug!(server = %server, window = lane.window, "upload queue: lane window set");
    }

    /// Queue `job` at the back of its tier on `server`'s lane, opening the lane (window one)
    /// if it has none.
    pub fn enqueue(&mut self, server: &ServerKey, job: QueuedJob<J>) {
        let lane = self.lane_mut_or_open(server);
        tracing::trace!(server = %server, tier = ?job.tier, bytes = job.bytes, "upload queue: job queued");
        lane.queued[tier_slot(job.tier)].push_back(job);
    }

    /// Replace everything `server`'s lane holds with `jobs`, derived from that server's own
    /// truth on resume. Any job still in flight on the lane is superseded and its reservation
    /// released (the caller has stopped it); its id is returned and later reports on it are
    /// ignored. The lane's window is kept, and so is any pause still running (a server's
    /// `Retry-After` binds whatever work the lane holds).
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
        let released: u64 = superseded.iter().map(|grant| grant.reserved).sum();
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

    /// Hold `server`'s lane until `until` (its backoff, or the server's `Retry-After`). Other
    /// lanes are served meanwhile. A later deadline replaces an earlier one.
    pub fn defer(&mut self, server: &ServerKey, until: Instant) {
        let lane = self.lane_mut_or_open(server);
        lane.paused_until = Some(lane.paused_until.map_or(until, |held| held.max(until)));
        tracing::debug!(server = %server, "upload queue: lane deferred");
    }

    /// Admit the next job, if any lane can run one now, and reserve its bytes.
    ///
    /// Lanes are tried round-robin from the one after the lane served last. A lane is passed
    /// over while it is paused, has no queued job, has its window full, or its next job does
    /// not fit the budget that is left; a lane never skips past its own head, so tier order
    /// holds inside it.
    pub fn next(&mut self, now: Instant) -> Option<Grant<J>> {
        let count = self.lanes.len();
        for step in 0..count {
            let index = (self.cursor + step) % count;
            let free = self.budget - self.in_use;
            let budget = self.budget;
            let lane = &mut self.lanes[index];
            if lane.paused_at(now) || lane.in_flight.len() >= lane.window {
                continue;
            }
            let Some(head) = lane.head() else { continue };
            let reserved = head.bytes.min(budget);
            if reserved > free {
                tracing::trace!(
                    server = %lane.server,
                    reserved,
                    free,
                    "upload queue: head does not fit the budget left; passing the lane over"
                );
                continue;
            }
            let job = lane.pop_head()?;
            let id = GrantId(self.next_grant);
            self.next_grant += 1;
            lane.paused_until = None;
            lane.last_progress = Some(now);
            lane.in_flight.push(InFlight {
                id,
                reserved,
                job: job.clone(),
            });
            let server = lane.server.clone();
            self.in_use += reserved;
            self.cursor = (index + 1) % count;
            tracing::debug!(
                server = %server,
                grant = id.0,
                tier = ?job.tier,
                reserved,
                in_use = self.in_use,
                "upload queue: job admitted"
            );
            return Some(Grant {
                id,
                server,
                tier: job.tier,
                reserved,
                job: job.job,
            });
        }
        None
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

    /// Grant `id` finished (sent, merged, or failed for good): release its reservation.
    /// Returns whether the grant was still in flight.
    pub fn complete(&mut self, id: GrantId, now: Instant) -> bool {
        let Some((lane, flight)) = self.take(id) else {
            return false;
        };
        let lane = &mut self.lanes[lane];
        lane.last_progress = Some(now);
        tracing::debug!(
            server = %lane.server,
            grant = id.0,
            released = flight.reserved,
            in_use = self.in_use,
            "upload queue: job complete"
        );
        true
    }

    /// Grant `id` must run again (its transfer was interrupted): release its reservation and
    /// return its job to the front of its tier. Returns whether the grant was still in flight.
    pub fn requeue(&mut self, id: GrantId) -> bool {
        let Some((lane, flight)) = self.take(id) else {
            return false;
        };
        let lane = &mut self.lanes[lane];
        tracing::debug!(server = %lane.server, grant = id.0, "upload queue: job requeued");
        lane.queued[tier_slot(flight.job.tier)].push_front(flight.job);
        true
    }

    /// Take the budget back from every lane whose jobs in flight have made no progress for the
    /// stall bound. Each such lane's jobs return to the front of their tiers, and the lane is
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
                released += flight.reserved;
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
            self.lanes.push(Lane::new(server.clone(), 1));
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

    /// Remove grant `id` from flight and release its reservation.
    fn take(&mut self, id: GrantId) -> Option<(usize, InFlight<J>)> {
        let (lane, at) = self.locate(id)?;
        // `remove`, not `swap_remove`: flights stay in grant order for `reclaim_stalled`.
        let flight = self.lanes[lane].in_flight.remove(at);
        self.in_use -= flight.reserved;
        Some((lane, flight))
    }
}

#[cfg(test)]
mod tests;
