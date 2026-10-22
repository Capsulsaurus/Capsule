//! What a peering session enforces after the handshake, and what guards the handshake itself.
//!
//! - [`DirectoryVersions`] is the persisted anti-rollback floor: the highest `directory_version`
//!   this device has verified. Persistence is local state, so it lives behind this seam;
//!   [`MemoryDirectoryVersions`] is the in-process adapter.
//! - [`HandshakeLimiter`] caps handshake attempts per source address, so the LAN cannot probe a
//!   device for membership. Time is passed in, so every rule is a deterministic unit test.
//! - [`PeerSession`] re-checks the peer's revocation on **every request**, not only at the
//!   handshake, and remembers what the peer delivered and when, so everything delivered after
//!   its revocation took effect can be quarantined.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use uuid::Uuid;

use super::PeeringError;
use super::channel::{PinnedTrust, VerifiedPeer, admit_device, admit_directory};

/// The persisted anti-rollback floor: the highest device-directory version this device has
/// verified. Implementations must be monotonic — [`record`](Self::record) never lowers it — and
/// durable across restarts, or a reboot readmits a stale directory.
pub trait DirectoryVersions: Send + Sync {
    /// The highest version recorded so far (`0` when none).
    fn highest(&self) -> Result<u64, PeeringError>;

    /// Record that `version` verified; the floor becomes `max(floor, version)`.
    fn record(&self, version: u64) -> Result<(), PeeringError>;
}

/// An in-memory [`DirectoryVersions`]: monotonic for the life of the process.
#[derive(Debug, Default)]
pub struct MemoryDirectoryVersions(AtomicU64);

impl MemoryDirectoryVersions {
    /// A floor starting at `version` — what a device restores from its persisted value.
    #[must_use]
    pub fn starting_at(version: u64) -> Self {
        Self(AtomicU64::new(version))
    }
}

impl DirectoryVersions for MemoryDirectoryVersions {
    fn highest(&self) -> Result<u64, PeeringError> {
        Ok(self.0.load(Ordering::Acquire))
    }

    fn record(&self, version: u64) -> Result<(), PeeringError> {
        self.0.fetch_max(version, Ordering::AcqRel);
        Ok(())
    }
}

/// Handshake attempts admitted per source address within one window, by default.
pub const DEFAULT_HANDSHAKES_PER_WINDOW: usize = 8;

/// The default rate-limit window.
pub const DEFAULT_HANDSHAKE_WINDOW: Duration = Duration::from_secs(60);

/// The most source addresses the limiter tracks at once, bounding its memory. Idle sources are
/// pruned when the table is full; a new source that still finds it full is refused.
pub const MAX_TRACKED_SOURCES: usize = 1024;

/// A sliding-window handshake rate limit per source address.
#[derive(Debug)]
pub struct HandshakeLimiter {
    max_attempts: usize,
    window: Duration,
    attempts: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl Default for HandshakeLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_HANDSHAKES_PER_WINDOW, DEFAULT_HANDSHAKE_WINDOW)
    }
}

impl HandshakeLimiter {
    /// Admit at most `max_attempts` handshakes per source address in any `window`.
    #[must_use]
    pub fn new(max_attempts: usize, window: Duration) -> Self {
        Self {
            max_attempts,
            window,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a handshake from `source` at `now` is admitted. An admitted attempt counts against
    /// the window; a refused one does not extend it. The table never holds more than
    /// [`MAX_TRACKED_SOURCES`] sources: when it is full, idle sources are pruned, and a new source
    /// that still finds it full of live ones is refused. A poisoned lock refuses too (fail closed).
    pub fn admit(&self, source: IpAddr, now: Instant) -> bool {
        let Ok(mut attempts) = self.attempts.lock() else {
            return false;
        };
        let window = self.window;
        let live = |at: &Instant| now.saturating_duration_since(*at) < window;
        if !attempts.contains_key(&source) && attempts.len() >= MAX_TRACKED_SOURCES {
            attempts.retain(|_, seen| seen.iter().any(live));
            if attempts.len() >= MAX_TRACKED_SOURCES {
                tracing::warn!(%source, "handshake limiter full of live sources; refusing");
                return false;
            }
        }
        let seen = attempts.entry(source).or_default();
        while seen.front().is_some_and(|at| !live(at)) {
            seen.pop_front();
        }
        if seen.len() >= self.max_attempts {
            tracing::debug!(%source, attempts = seen.len(), "handshake attempt over the limit");
            return false;
        }
        seen.push_back(now);
        true
    }

    /// How many sources the table tracks — the bound tests assert.
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.attempts.lock().map_or(0, |attempts| attempts.len())
    }
}

/// One established peering session, as the serving side tracks it: the verified peer, whether
/// it has been cut off, and what it delivered when.
#[derive(Debug)]
pub struct PeerSession {
    peer: Uuid,
    cut_off: bool,
    deliveries: Vec<(Uuid, Timestamp)>,
}

impl PeerSession {
    /// A session with the peer the handshake verified.
    #[must_use]
    pub fn new(peer: &VerifiedPeer) -> Self {
        Self {
            peer: peer.device_id,
            cut_off: false,
            deliveries: Vec::new(),
        }
    }

    /// The peer's directory device id.
    #[must_use]
    pub fn peer(&self) -> Uuid {
        self.peer
    }

    /// Authorize the peer's next request against the trust pinned **now**: the directory must
    /// still chain to the IK without rolling back, and the peer must still be enrolled and
    /// un-revoked. A refusal cuts the session off for good, so a later request is refused even if
    /// a caller retries with older trust.
    pub fn authorize(
        &mut self,
        trust: &PinnedTrust,
        versions: &dyn DirectoryVersions,
    ) -> Result<(), PeeringError> {
        if self.cut_off {
            return Err(PeeringError::RevokedDevice(self.peer));
        }
        let verdict = admit_directory(trust, versions).and_then(|()| {
            admit_device(trust, self.peer)?;
            Ok(())
        });
        if let Err(error) = &verdict {
            tracing::warn!(peer = %self.peer, %error, "peer request refused; session cut off");
            self.cut_off = true;
        }
        verdict
    }

    /// Record that the peer delivered `asset` at `received_at`.
    pub fn record_delivery(&mut self, asset: Uuid, received_at: Timestamp) {
        self.deliveries.push((asset, received_at));
    }

    /// The assets this peer delivered at or after its revocation took effect under `trust` —
    /// everything to quarantine. Empty while the peer is un-revoked. A peer absent from the
    /// directory, or revoked at a time that does not parse, has every delivery quarantined
    /// (fail closed).
    #[must_use]
    pub fn quarantined_deliveries(&self, trust: &PinnedTrust) -> Vec<Uuid> {
        let effective = match trust.directory.device(&self.peer) {
            Some(entry) => match entry.revoked_at.as_deref() {
                None => return Vec::new(),
                Some(at) => at.parse::<Timestamp>().ok(),
            },
            None => None,
        };
        let quarantined: Vec<Uuid> = self
            .deliveries
            .iter()
            .filter(|(_, at)| effective.is_none_or(|revoked| *at >= revoked))
            .map(|(asset, _)| *asset)
            .collect();
        tracing::info!(
            peer = %self.peer,
            quarantined = quarantined.len(),
            "quarantining deliveries made after the peer's revocation"
        );
        quarantined
    }
}
