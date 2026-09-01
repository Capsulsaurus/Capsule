//! Blob garbage-collection state and the **GC grace window** — the seam between storage
//! verification (slice `S-C3`, which *reads* this state) and the byte-deletion GC worker
//! (slice `S-C11`, which *owns and writes* it).
//!
//! ## What lives here
//!
//! - [`BlobGcState`] — the per-blob liveness facts the `retrievable` verdict consumes:
//!   whether a blob is mid-collection (`collectable_since`) or quarantined for an integrity
//!   fault. A blob with **no** row is live and un-quarantined by construction.
//! - [`GC_GRACE_WINDOW`] + [`earliest_byte_deletion`] — the standing grace contract that
//!   makes verify-before-destroy sound without a lease protocol.
//!
//! ## The GC-grace contract (what `S-C11` MUST honor)
//!
//! Storage verification's [`Verify Before Destroy`] rule requires that *"a blob that just
//! answered `durable` cannot reach byte deletion faster than the standing GC grace
//! window."* The verify endpoint deliberately **writes no state** (it is a pure read), so
//! the guarantee is structural rather than a per-request lease:
//!
//! 1. A `durable` / `retrievable` verdict requires `collectable_since IS NULL` at
//!    `checked_at` — [`BlobGcState::is_retrievable`] returns `false` the instant a blob is
//!    marked collectable.
//! 2. The GC worker (`S-C11`) MUST NOT byte-delete a blob before
//!    [`earliest_byte_deletion`]`(collectable_since)` — i.e. it holds a collectable blob's
//!    bytes for at least [`GC_GRACE_WINDOW`] after marking it.
//!
//! Composed: a blob that answered `durable` at `checked_at` had `collectable_since = None`,
//! so any later mark is strictly after `checked_at`, and its bytes then survive a further
//! `GC_GRACE_WINDOW`. The client's bounded verify→release window (default 60 s) fits inside
//! that grace, so the release is safe against a racing GC pass without any server-side
//! write on the verify path. `S-C11` finds this contract here and gates its deletion sweep
//! on [`earliest_byte_deletion`].
//!
//! SSoT: [Storage Verification — Verify Before Destroy] and
//! [Filesystem — Deletion and Garbage Collection].
//!
//! ## The write side (`S-C11`)
//!
//! The read contract above is consumed by [`worker`] (the two-phase refcount mark-and-sweep
//! over the blob store plus the orphan/dangling sweep) and [`retention`] (the keyless
//! retention purge worker that enforces the signed `retention_until` floor). Both are
//! operator-invokable — no scheduling framework, a plain callable a binary can cron — and
//! both take a [`Clock`] seam so their time-based behaviour (the grace window, the retention
//! window) is proven deterministically with an injected clock, never a sleep.
//!
//! [`Verify Before Destroy`]: ../../../../capsule-docs/src/content/docs/design/import/storage-verification.md
//! [Storage Verification — Verify Before Destroy]: ../../../../capsule-docs/src/content/docs/design/import/storage-verification.md
//! [Filesystem — Deletion and Garbage Collection]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md

use std::collections::HashMap;

use ::entity::blob_gc;
use ::entity::time::entity_tz_to_ts;
use jiff::{SignedDuration, Timestamp};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub mod retention;
pub mod worker;

pub use retention::{RetentionPurgeWorker, RetentionReport};
pub use worker::{GcReport, GcWorker, reference_count};

/// A trusted-clock seam so the GC grace window and the retention window are exercised
/// deterministically (an injected clock in tests, the system clock in production) — never a
/// sleep. Mirrors the `S-C3` verify service's `Clock` and the `S-D7` pattern.
///
/// This is the server's own trusted clock — the authoritative instant for all time-based
/// deletion policy, per [Filesystem — What the server knows][clk]. It is compared against the
/// signed `retention_until` and the recorded `collectable_since`; it never trusts a
/// client-asserted time.
///
/// [clk]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md
pub trait Clock: Send + Sync {
    /// The current trusted-server instant.
    fn now(&self) -> Timestamp;
}

/// The production [`Clock`], backed by the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// The standing grace window between a blob becoming collectable and its bytes being
/// eligible for deletion. Comfortably larger than the client's bounded verify→release
/// window (default 60 s) so a `durable` verdict is never invalidated by a racing GC pass.
pub const GC_GRACE_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// The earliest instant the GC worker (`S-C11`) may byte-delete a blob that became
/// collectable at `collectable_since`. The deletion sweep MUST treat a blob as *retained*
/// until `now >= earliest_byte_deletion(collectable_since)`.
#[must_use]
pub fn earliest_byte_deletion(collectable_since: Timestamp) -> Timestamp {
    collectable_since
        .checked_add(GC_GRACE_WINDOW)
        .unwrap_or(Timestamp::MAX)
}

/// The per-blob GC facts the `retrievable` verdict reads. Absence of a row means a live,
/// referenced, un-quarantined blob — the common case carries no GC row at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlobGcState {
    /// Set once the GC worker marks the blob unreferenced (refcount reached 0). While set
    /// the blob is mid-collection and **not** retrievable; its bytes survive until
    /// [`earliest_byte_deletion`].
    pub collectable_since: Option<Timestamp>,
    /// Set when an integrity fault quarantines the blob (dangling reference, failed deep
    /// scan). A quarantined blob is never retrievable.
    pub quarantined: bool,
}

impl BlobGcState {
    /// Whether this blob is in a state the server would actually serve: not mid-collection
    /// and not quarantined. (`refcount > 0` is established by the index reference, checked
    /// separately as the `indexed` fact.)
    #[must_use]
    pub fn is_retrievable(&self) -> bool {
        self.collectable_since.is_none() && !self.quarantined
    }
}

/// Read-only access to blob GC state, for the storage-verification `retrievable` check.
pub struct Query;

impl Query {
    /// Load the GC state of each requested content hash. Hashes without a `blob_gc` row are
    /// omitted from the map — callers treat a missing entry as the default (live,
    /// un-quarantined) [`BlobGcState`].
    pub async fn blob_states<C: ConnectionTrait>(
        db: &C,
        hashes: &[String],
    ) -> Result<HashMap<String, BlobGcState>, DbErr> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = blob_gc::Entity::find()
            .filter(blob_gc::Column::ContentHash.is_in(hashes.iter().cloned()))
            .all(db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let state = BlobGcState {
                    collectable_since: row.collectable_since.map(entity_tz_to_ts),
                    quarantined: row.quarantined,
                };
                (row.content_hash, state)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_window_exceeds_client_release_window() {
        // The client re-verifies if >60 s elapse between verdict and release; the server's
        // grace must comfortably exceed that so a durable verdict is never raced by GC.
        assert!(GC_GRACE_WINDOW > SignedDuration::from_secs(60));
    }

    #[test]
    fn deletion_is_gated_a_full_grace_window_after_marking() {
        let marked: Timestamp = "2026-07-10T00:00:00Z".parse().unwrap();
        assert_eq!(
            earliest_byte_deletion(marked),
            marked.checked_add(GC_GRACE_WINDOW).unwrap(),
        );
    }

    #[test]
    fn a_blob_that_answered_durable_survives_the_release_window() {
        // A durable verdict at `checked_at` implies collectable_since was None then, so any
        // later GC mark is strictly after checked_at — and deletion is a further grace
        // window out. The client's release window is bounded well inside that.
        let checked_at: Timestamp = "2026-07-10T00:00:00Z".parse().unwrap();
        let release_deadline = checked_at
            .checked_add(SignedDuration::from_secs(60))
            .unwrap();
        // Worst case: GC marks the blob the instant after the verdict.
        let marked_after = checked_at
            .checked_add(SignedDuration::from_nanos(1))
            .unwrap();
        assert!(earliest_byte_deletion(marked_after) > release_deadline);
    }

    #[test]
    fn default_state_is_retrievable_only_when_clean() {
        assert!(BlobGcState::default().is_retrievable());
        assert!(
            !BlobGcState {
                quarantined: true,
                ..Default::default()
            }
            .is_retrievable()
        );
        let ts: Timestamp = "2026-07-10T00:00:00Z".parse().unwrap();
        assert!(
            !BlobGcState {
                collectable_since: Some(ts),
                quarantined: false
            }
            .is_retrievable()
        );
    }
}
