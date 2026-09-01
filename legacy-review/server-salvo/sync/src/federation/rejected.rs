//! The soft-fail rejected-hash table (slice `S-E2`; SSoT:
//! [Federation — Soft-Fail Semantics](https://docs/design/federation/#soft-fail-semantics)).
//!
//! A federated event that fails validation is rejected **locally** — not applied, no authority
//! derived — but its hash is **remembered**, so Capsule's view does not silently diverge from a
//! peer that (wrongly) accepted it. Divergence is the real enemy; explicit rejection-with-memory
//! is the cure.
//!
//! A hostile peer could flood the table, so it is **bounded**: a cap (default 100 000) with a
//! per-entry TTL (default 90 days), both deployment-configurable. Eviction is **LRU by last
//! reference** within the cap — the hashes that age out are the ones not referenced again, so by
//! the time they age out they are no longer load-bearing for divergence detection.

use indexmap::IndexMap;
use jiff::{SignedDuration, Timestamp};

/// Default maximum number of remembered rejected hashes.
pub const DEFAULT_CAP: usize = 100_000;

/// Default per-entry time-to-live.
#[must_use]
pub fn default_ttl() -> SignedDuration {
    SignedDuration::from_hours(24 * 90)
}

/// A bounded, LRU-by-last-reference, TTL'd set of rejected content hashes.
///
/// Insertion order in the backing map is the LRU order: the front is the least-recently
/// referenced, the back the most recent. Referencing a hash (remembering it again, or a
/// `contains` hit) moves it to the back.
pub struct RejectedHashTable {
    cap: usize,
    ttl: SignedDuration,
    entries: IndexMap<String, Timestamp>,
}

impl RejectedHashTable {
    /// Build a table with an explicit cap and TTL.
    #[must_use]
    pub fn new(cap: usize, ttl: SignedDuration) -> Self {
        Self {
            cap: cap.max(1),
            ttl,
            entries: IndexMap::new(),
        }
    }

    /// Build a table with the default cap (100 000) and TTL (90 days).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_CAP, default_ttl())
    }

    /// Remember a rejected `hash` as of `now`. Refreshes its last-reference (moving it to MRU) if
    /// already present; otherwise inserts it, evicting the least-recently-referenced entry first
    /// if the table is at cap. Expired entries are pruned on the way in.
    #[tracing::instrument(skip(self), fields(hash = %hash))]
    pub fn remember(&mut self, hash: &str, now: Timestamp) {
        self.prune_expired(now);
        if self.entries.shift_remove(hash).is_some() {
            self.entries.insert(hash.to_string(), now);
            return;
        }
        if self.entries.len() >= self.cap {
            // Evict the least-recently-referenced entry (front of the insertion order).
            self.entries.shift_remove_index(0);
        }
        self.entries.insert(hash.to_string(), now);
        tracing::debug!(
            len = self.entries.len(),
            "remembered rejected hash (soft-fail)"
        );
    }

    /// Whether `hash` is currently remembered (as of `now`, honoring the TTL). A hit refreshes the
    /// entry's last-reference.
    pub fn contains(&mut self, hash: &str, now: Timestamp) -> bool {
        self.prune_expired(now);
        if self.entries.shift_remove(hash).is_some() {
            self.entries.insert(hash.to_string(), now);
            true
        } else {
            false
        }
    }

    /// The number of remembered hashes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every entry older than the TTL. Order-preserving, so LRU order survives.
    fn prune_expired(&mut self, now: Timestamp) {
        let ttl_secs = self.ttl.as_secs();
        self.entries
            .retain(|_, last_ref| now.as_second() - last_ref.as_second() <= ttl_secs);
    }
}
