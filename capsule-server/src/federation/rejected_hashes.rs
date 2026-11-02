//! [`RejectedHashes`]: the bounded soft-fail table (design/federation.md, Soft-Fail Semantics).
//!
//! # What it remembers, and why
//!
//! A federated event that fails validation is not applied, not shown and grants no authority,
//! but its hash is **remembered**, so this server's view does not silently diverge from a peer
//! that accepted it. The puller records here the manifest a home server served after the album
//! had moved past it (the [stale-revival defense](super::pull::pulled::AssetHead::superseded)):
//! the record's hash, the peer and album it came from, and the rule it broke.
//!
//! # Bounded, by count and by age
//!
//! A peer chooses what it serves, so a table it can grow is a table it can exhaust. The table
//! holds at most [`RejectedHashBounds::capacity`] rows (default
//! [`DEFAULT_REJECTED_HASH_CAPACITY`]) and forgets a row once it has gone
//! [`RejectedHashBounds::ttl`] (default [`DEFAULT_REJECTED_HASH_TTL_DAYS`] days) without a
//! reference. A **reference** is a [`record`](RejectedHashes::record) of a hash the table
//! already holds: the same rejected event presented again. A [`lookup`](RejectedHashes::lookup)
//! is a read and references nothing, so a reader can never keep a row alive. Past capacity the
//! row referenced longest ago is evicted, ties broken by the smaller hash, so both adapters
//! evict the same row.
//!
//! # Eviction loses nothing the system needs
//!
//! A row is a memory of a refusal and nothing else: no blob, entry or head is held through it,
//! and nothing is applied because of it. The refusal itself does not depend on the table either:
//! the asset's superseded heads, which [`PulledAlbums`](super::pull::PulledAlbums) keeps, are
//! what refuses a revival. Evicting a row forgets that a refusal happened; it deletes no data
//! and admits nothing that was refused.
//!
//! # What lives where
//!
//! - This file — the port, its records, [`RejectedHashBounds`] and [`InMemoryRejectedHashes`],
//!   the deterministic double.
//! - [`postgres`] — [`PostgresRejectedHashes`](postgres::PostgresRejectedHashes), the durable
//!   adapter over `federation_rejected_hashes`.
//! - [`conformance`] — the suite both adapters pass.
//!
//! # Times are the caller's
//!
//! Every operation takes the instant it decides at, so both adapters are driven through one
//! suite with absolute instants.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::{Mutex, PoisonError};

use capsule_core::crypto::hash::Hash32;
use jiff::{SignedDuration, Timestamp};

use super::PeerId;
use crate::store::{AlbumId, StoreFuture};

pub mod conformance;
pub mod postgres;

/// The default number of rejected hashes the table holds: 100,000.
pub const DEFAULT_REJECTED_HASH_CAPACITY: u32 = 100_000;

/// The default number of days a rejected hash is kept without a reference: 90.
pub const DEFAULT_REJECTED_HASH_TTL_DAYS: u32 = 90;

/// How much the table holds, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedHashBounds {
    capacity: NonZeroU32,
    ttl_days: NonZeroU32,
}

impl RejectedHashBounds {
    /// At most `capacity` rows, each forgotten `ttl_days` days after its last reference.
    pub const fn new(capacity: NonZeroU32, ttl_days: NonZeroU32) -> Self {
        Self { capacity, ttl_days }
    }

    /// These bounds, holding at most `capacity` rows.
    #[must_use]
    pub const fn with_capacity(self, capacity: NonZeroU32) -> Self {
        Self { capacity, ..self }
    }

    /// These bounds, keeping a row `ttl_days` days after its last reference.
    #[must_use]
    pub const fn with_ttl_days(self, ttl_days: NonZeroU32) -> Self {
        Self { ttl_days, ..self }
    }

    /// The most rows the table holds.
    pub const fn capacity(self) -> u32 {
        self.capacity.get()
    }

    /// How long a row is kept without a reference.
    pub fn ttl(self) -> SignedDuration {
        SignedDuration::from_hours(24 * i64::from(self.ttl_days.get()))
    }

    /// The last-reference instant at or before which a row is forgotten at `now`.
    ///
    /// A row referenced at `at` is kept while `now < at + ttl`, so it is expired once `at` is at
    /// or before `now - ttl`. Saturates at the start of time rather than failing.
    pub fn expired_at_or_before(self, now: Timestamp) -> Timestamp {
        now.checked_sub(self.ttl()).unwrap_or(Timestamp::MIN)
    }
}

impl Default for RejectedHashBounds {
    fn default() -> Self {
        Self::new(
            NonZeroU32::new(DEFAULT_REJECTED_HASH_CAPACITY).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(DEFAULT_REJECTED_HASH_TTL_DAYS).unwrap_or(NonZeroU32::MIN),
        )
    }
}

/// One refusal to remember.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The refused event's hash: for a manifest, its record hash.
    pub hash: Hash32,
    /// The peer that presented it.
    pub peer: PeerId,
    /// The album it was presented in.
    pub album: AlbumId,
    /// The asset it names.
    pub asset_id: String,
    /// The rule it broke, as the log names it (`invariant 17`).
    pub rule: String,
}

/// A remembered refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedHash {
    /// The refusal as it was first recorded. A later presentation of the same hash moves only
    /// the reference fields below.
    pub rejection: Rejection,
    /// When it was first refused.
    pub first_rejected_at: Timestamp,
    /// When it was last presented: what its age and its eviction order are counted from.
    pub last_referenced_at: Timestamp,
    /// How many times it has been presented, the first included.
    pub references: u64,
}

/// Where rejected hashes are remembered.
pub trait RejectedHashes: std::fmt::Debug + Send + Sync {
    /// Remember `rejection` at `now`: a new row, or a reference to the row its hash already has.
    ///
    /// A reference moves the row's last reference to `now` (never backwards) and counts it,
    /// keeping its first attribution. Then every row past its age is forgotten, and rows past
    /// capacity are evicted, the one referenced longest ago first, so the table never holds
    /// more than its capacity after a record returns.
    fn record(&self, rejection: Rejection, now: Timestamp) -> StoreFuture<'_, ()>;

    /// The remembered refusal of `hash`, unless it is past its age at `now`. References nothing.
    fn lookup<'a>(
        &'a self,
        hash: &'a Hash32,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<RejectedHash>>;
}

/// The deterministic double of [`RejectedHashes`].
///
/// Instants are kept to the microsecond, as the durable adapter stores them, so both answer the
/// same instants back.
#[derive(Debug, Default)]
pub struct InMemoryRejectedHashes {
    bounds: RejectedHashBounds,
    table: Mutex<Table>,
}

#[derive(Debug, Default)]
struct Table {
    rows: BTreeMap<Hash32, RejectedHash>,
    /// Every row by `(last_referenced_at, hash)`: the eviction order, oldest first.
    order: BTreeSet<(Timestamp, Hash32)>,
}

impl Table {
    fn remove(&mut self, hash: &Hash32) {
        if let Some(row) = self.rows.remove(hash) {
            self.order.remove(&(row.last_referenced_at, *hash));
        }
    }
}

impl InMemoryRejectedHashes {
    /// An empty table bounded by `bounds`.
    pub fn new(bounds: RejectedHashBounds) -> Self {
        Self {
            bounds,
            table: Mutex::default(),
        }
    }

    /// How many rows the table holds, expired ones included until the next record.
    pub fn len(&self) -> usize {
        self.lock().rows.len()
    }

    /// Whether the table holds no row.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `at` to the microsecond, as a `BIGINT` of microseconds holds it.
fn to_the_microsecond(at: Timestamp) -> Timestamp {
    crate::postgres::time::stored(at)
}

impl RejectedHashes for InMemoryRejectedHashes {
    fn record(&self, rejection: Rejection, now: Timestamp) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let now = to_the_microsecond(now);
            let horizon = self.bounds.expired_at_or_before(now);
            let mut table = self.lock();
            let hash = rejection.hash;
            // A row past its age is forgotten before it could be referenced: the hash is
            // remembered afresh, as the durable adapter's sweep-then-upsert does.
            if table
                .rows
                .get(&hash)
                .is_some_and(|held| held.last_referenced_at <= horizon)
            {
                table.remove(&hash);
            }
            let referenced = table.rows.get(&hash).cloned().map(|mut row| {
                table.order.remove(&(row.last_referenced_at, hash));
                row.last_referenced_at = row.last_referenced_at.max(now);
                row.references = row.references.saturating_add(1);
                row
            });
            let referenced_again = referenced.is_some();
            let row = referenced.unwrap_or(RejectedHash {
                rejection,
                first_rejected_at: now,
                last_referenced_at: now,
                references: 1,
            });
            table.order.insert((row.last_referenced_at, hash));
            table.rows.insert(hash, row);

            let expired: Vec<Hash32> = table
                .order
                .iter()
                .take_while(|(at, _)| *at <= horizon)
                .map(|(_, hash)| *hash)
                .collect();
            let capacity = usize::try_from(self.bounds.capacity()).unwrap_or(usize::MAX);
            let over = table
                .rows
                .len()
                .saturating_sub(expired.len())
                .saturating_sub(capacity);
            let evicted: Vec<Hash32> = table
                .order
                .iter()
                .skip(expired.len())
                .take(over)
                .map(|(_, hash)| *hash)
                .collect();
            for hash in expired.iter().chain(&evicted) {
                table.remove(hash);
            }
            tracing::debug!(
                hash = %hash.to_hex(),
                referenced_again,
                expired = expired.len(),
                evicted = evicted.len(),
                held = table.rows.len(),
                "a rejected hash was recorded"
            );
            Ok(())
        })
    }

    fn lookup<'a>(
        &'a self,
        hash: &'a Hash32,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<RejectedHash>> {
        Box::pin(async move {
            let horizon = self.bounds.expired_at_or_before(to_the_microsecond(now));
            Ok(self
                .lock()
                .rows
                .get(hash)
                .filter(|row| row.last_referenced_at > horizon)
                .cloned())
        })
    }
}

#[cfg(test)]
mod tests;
