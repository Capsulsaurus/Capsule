//! The blob-store garbage collector — the **write** side of `service::gc` (slice `S-C11`).
//!
//! A two-phase mark-and-sweep over reference counts, deliberately built so a bug biases
//! toward *keeping* bytes, never deleting live ones (the [data-integrity principle]):
//!
//! - **Mark.** Walk the content-addressed blob store; for every blob physically present,
//!   count its live references ([`reference_count`]). A blob with **zero** references is
//!   marked collectable (`blob_gc.collectable_since := now`), starting the grace clock. A
//!   blob whose reference *reappeared* during the grace window — an in-flight finalization
//!   retry, a concurrent merge — has its mark cancelled. This same pass reclaims the
//!   **finalization-crash orphan** (a blob renamed into `blobs/` whose Postgres commit never
//!   landed) because such a blob is, by construction, referenced by nothing.
//! - **Sweep.** For each marked, un-quarantined blob whose grace window has elapsed
//!   ([`super::earliest_byte_deletion`]), re-confirm zero references **inside the deleting
//!   transaction** (under the `blob_gc` row lock) and only then byte-delete. The grace gate
//!   is the [`S-C3` verify-before-destroy contract](super): a blob that just answered
//!   `durable` had `collectable_since = None`, so any later mark is strictly after that
//!   verdict, and its bytes then survive a further `GC_GRACE_WINDOW`.
//!
//! The asymmetric direction — a committed row pointing at a blob **missing** from the store —
//! is a *loud* integrity error: the referencing row is **never** auto-deleted (erasing it
//! would destroy the only record the asset should exist), the hash is **never** treated as
//! collectable, and the blob is quarantined for an operator. A [dry run](GcWorker::mark_and_sweep)
//! reports what *would* be collected without removing anything.
//!
//! SSoT: [Filesystem — Deletion and Garbage Collection].
//!
//! [data-integrity principle]: ../../../../capsule-docs/src/content/docs/design/principles.md
//! [Filesystem — Deletion and Garbage Collection]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md

use std::collections::BTreeSet;
use std::path::PathBuf;

use ::entity::time::{entity_tz_to_ts, ts_to_entity_tz};
use ::entity::{asset, blob_gc, quota_ledger};
use jiff::Timestamp;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    PaginatorTrait, QueryFilter, QuerySelect, Set, TransactionTrait,
};
use tracing::{debug, info, instrument, warn};

use super::{Clock, SystemClock, earliest_byte_deletion};
use crate::blob_store;

/// The **live** reference count of a blob content address — the single source of truth for GC
/// eligibility ([Filesystem — Deletion and GC][doc]). A blob is GC-eligible only when this
/// returns zero.
///
/// References are counted over the two committed sources the write paths actually maintain
/// (never a separately-drifting counter):
///
/// - **originals** — one per `assets` row whose `file_hash` is this address. A trash-retained
///   (soft-deleted) asset still holds its row, so it still counts; only a hard purge (the
///   [retention worker](super::retention)) drops the row and releases the reference.
/// - **auxiliary blobs** (metadata / derivative / provenance / federated cache) — the
///   [`quota_ledger`] refcount for this address, which `S-C6` decrements on release and
///   deletes at zero.
///
/// [doc]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md
/// [`quota_ledger`]: entity::quota_ledger
pub async fn reference_count<C: ConnectionTrait>(db: &C, hash: &str) -> Result<u64, DbErr> {
    let asset_refs = asset::Entity::find()
        .filter(asset::Column::FileHash.eq(hash))
        .count(db)
        .await?;
    let ledger_refs = match quota_ledger::Entity::find_by_id(hash).one(db).await? {
        Some(row) => u64::try_from(row.refcount.max(0)).unwrap_or(0),
        None => 0,
    };
    Ok(asset_refs + ledger_refs)
}

/// A structured summary of one GC pass, for operator visibility and dry-run inspection. Every
/// count is also logged individually (per the traceability principle); this is the roll-up.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// Blob files scanned in the content-addressed store.
    pub scanned: usize,
    /// Zero-reference blobs newly marked collectable this pass (includes reclaimed
    /// finalization-crash orphans).
    pub marked: usize,
    /// Marks cancelled because a reference reappeared during the grace window.
    pub cancelled: usize,
    /// Dangling references quarantined (a committed row → a blob missing from the store).
    pub dangling_quarantined: usize,
    /// Blobs whose grace window has **not** elapsed — retained this pass.
    pub retained_in_grace: usize,
    /// Blobs byte-deleted (or, in a dry run, that *would* be deleted).
    pub swept: usize,
    /// Total bytes reclaimed by the sweep (zero in a dry run).
    pub swept_bytes: u64,
    /// Whether this was a non-mutating dry run.
    pub dry_run: bool,
}

/// The operator-invokable blob garbage collector over one content-addressed blob store.
///
/// Construct with [`GcWorker::new`] for production, or [`GcWorker::with_clock`] to inject a
/// deterministic [`Clock`] in tests. A binary crons [`GcWorker::mark_and_sweep`]; there is no
/// scheduling framework here.
pub struct GcWorker<K: Clock = SystemClock> {
    upload_dir: PathBuf,
    clock: K,
}

impl GcWorker<SystemClock> {
    /// A GC worker over `upload_dir`'s blob store, on the system clock.
    #[must_use]
    pub fn new(upload_dir: PathBuf) -> Self {
        Self {
            upload_dir,
            clock: SystemClock,
        }
    }
}

impl<K: Clock> GcWorker<K> {
    /// A GC worker with an injected [`Clock`] — the seam that proves the grace window without
    /// sleeping.
    pub fn with_clock(upload_dir: PathBuf, clock: K) -> Self {
        Self { upload_dir, clock }
    }

    /// Run one full mark-and-sweep pass (mark → sweep). With `dry_run`, marks/cancels/
    /// quarantines are applied (they only add safety) but **no bytes are deleted** — the sweep
    /// reports what it *would* remove. Returns the [`GcReport`] roll-up.
    #[instrument(skip(self, db), fields(upload_dir = %self.upload_dir.display(), dry_run))]
    pub async fn mark_and_sweep(
        &self,
        db: &DatabaseConnection,
        dry_run: bool,
    ) -> Result<GcReport, DbErr> {
        let now = self.clock.now();
        let mut report = GcReport {
            dry_run,
            ..Default::default()
        };
        info!(%now, dry_run, "gc: mark-and-sweep pass starting");
        self.mark(db, now, &mut report).await?;
        self.sweep(db, now, dry_run, &mut report).await?;
        info!(
            scanned = report.scanned,
            marked = report.marked,
            cancelled = report.cancelled,
            dangling = report.dangling_quarantined,
            retained_in_grace = report.retained_in_grace,
            swept = report.swept,
            swept_bytes = report.swept_bytes,
            dry_run,
            "gc: mark-and-sweep pass complete"
        );
        Ok(report)
    }

    /// The **mark** phase (and the orphan/dangling sweep the finalization crash-safety depends
    /// on). Marks every zero-reference blob collectable, cancels marks whose reference
    /// reappeared, and quarantines dangling references. Purely additive to safety — never
    /// deletes bytes — so it runs even under `dry_run`.
    async fn mark(
        &self,
        db: &DatabaseConnection,
        now: Timestamp,
        report: &mut GcReport,
    ) -> Result<(), DbErr> {
        let present = self
            .present_blob_hashes()
            .map_err(|e| DbErr::Custom(format!("scan blob store: {e}")))?;
        report.scanned = present.len();

        for hash in &present {
            let refs = reference_count(db, hash).await?;
            let existing = blob_gc::Entity::find_by_id(hash.clone()).one(db).await?;

            if refs == 0 {
                match existing {
                    // A quarantined blob is an operator's to resolve — never re-marked.
                    Some(row) if row.quarantined => {}
                    // Already marked: keep the original `collectable_since` so the grace clock
                    // is not extended pass after pass.
                    Some(row) if row.collectable_since.is_some() => {}
                    // A row exists (e.g. cleared quarantine) without a mark: set the clock.
                    Some(row) => {
                        let mut am: blob_gc::ActiveModel = row.into();
                        am.collectable_since = Set(Some(ts_to_entity_tz(now)));
                        am.update(db).await?;
                        report.marked += 1;
                        info!(%hash, refs, marked_at = %now, "gc: marked blob collectable (zero references)");
                    }
                    // The common case: no row means live-by-default, so a fresh mark is one insert.
                    None => {
                        blob_gc::ActiveModel {
                            content_hash: Set(hash.clone()),
                            collectable_since: Set(Some(ts_to_entity_tz(now))),
                            quarantined: Set(false),
                        }
                        .insert(db)
                        .await?;
                        report.marked += 1;
                        info!(%hash, refs, marked_at = %now, "gc: marked blob collectable (zero references)");
                    }
                }
            } else if let Some(row) = existing
                && !row.quarantined
                && row.collectable_since.is_some()
            {
                // A reference reappeared during the grace window — cancel the mark outright.
                blob_gc::Entity::delete_by_id(hash.clone()).exec(db).await?;
                report.cancelled += 1;
                info!(%hash, refs, "gc: cancelled mark — reference reappeared inside grace window");
            }
        }

        // Dangling direction: a committed `assets` row referencing a blob **missing** from the
        // store. Never auto-deleted, never collectable — surfaced loudly and quarantined.
        let referenced: Vec<String> = asset::Entity::find()
            .select_only()
            .column(asset::Column::FileHash)
            .distinct()
            .into_tuple()
            .all(db)
            .await?;
        for hash in referenced {
            if !present.contains(&hash) {
                self.quarantine_dangling(db, &hash, report).await?;
            }
        }
        Ok(())
    }

    /// Quarantine a dangling reference (a committed row → a blob missing from `blobs/`). This
    /// makes storage verification report the blob non-retrievable and preserves it for an
    /// operator; it **never** removes the referencing row and **never** marks the hash
    /// collectable.
    async fn quarantine_dangling(
        &self,
        db: &DatabaseConnection,
        hash: &str,
        report: &mut GcReport,
    ) -> Result<(), DbErr> {
        match blob_gc::Entity::find_by_id(hash.to_string())
            .one(db)
            .await?
        {
            Some(row) if row.quarantined => {} // already flagged
            Some(row) => {
                let mut am: blob_gc::ActiveModel = row.into();
                am.quarantined = Set(true);
                // A dangling reference is never collectable — clear any stale mark.
                am.collectable_since = Set(None);
                am.update(db).await?;
                report.dangling_quarantined += 1;
                warn!(%hash, "gc: DANGLING REFERENCE — committed row points at a blob missing from the store; quarantined, row preserved, never auto-deleted");
            }
            None => {
                blob_gc::ActiveModel {
                    content_hash: Set(hash.to_string()),
                    collectable_since: Set(None),
                    quarantined: Set(true),
                }
                .insert(db)
                .await?;
                report.dangling_quarantined += 1;
                warn!(%hash, "gc: DANGLING REFERENCE — committed row points at a blob missing from the store; quarantined, row preserved, never auto-deleted");
            }
        }
        Ok(())
    }

    /// The **sweep** phase: byte-delete every marked, un-quarantined blob whose grace window
    /// has elapsed, re-confirming zero references inside the deleting transaction.
    async fn sweep(
        &self,
        db: &DatabaseConnection,
        now: Timestamp,
        dry_run: bool,
        report: &mut GcReport,
    ) -> Result<(), DbErr> {
        // Quarantined blobs are excluded structurally — they are never candidates for deletion.
        let candidates = blob_gc::Entity::find()
            .filter(blob_gc::Column::CollectableSince.is_not_null())
            .filter(blob_gc::Column::Quarantined.eq(false))
            .all(db)
            .await?;

        for row in candidates {
            let Some(marked_tz) = row.collectable_since else {
                continue;
            };
            let marked = entity_tz_to_ts(marked_tz);
            if now < earliest_byte_deletion(marked) {
                report.retained_in_grace += 1;
                debug!(
                    hash = %row.content_hash,
                    marked = %marked,
                    earliest = %earliest_byte_deletion(marked),
                    %now,
                    "gc: retained — grace window has not elapsed"
                );
                continue;
            }

            // Grace elapsed: re-confirm zero references under the `blob_gc` row lock so a
            // concurrent mark cannot race the delete, and a reference that reappeared cancels it.
            let txn = db.begin().await?;
            let Some(locked) = blob_gc::Entity::find_by_id(row.content_hash.clone())
                .lock_exclusive()
                .one(&txn)
                .await?
            else {
                txn.rollback().await?;
                continue;
            };
            if locked.quarantined {
                txn.rollback().await?;
                continue;
            }
            let refs = reference_count(&txn, &locked.content_hash).await?;
            if refs > 0 {
                blob_gc::Entity::delete_by_id(locked.content_hash.clone())
                    .exec(&txn)
                    .await?;
                txn.commit().await?;
                report.cancelled += 1;
                info!(hash = %locked.content_hash, refs, "gc: sweep cancelled — reference reappeared under the deleting lock");
                continue;
            }

            if dry_run {
                txn.rollback().await?;
                report.swept += 1;
                info!(hash = %locked.content_hash, marked = %marked, "gc: [dry-run] would byte-delete collectable blob");
                continue;
            }

            // Byte-delete the file, then drop the GC row in the same transaction. If the file
            // is already gone (a prior interrupted sweep), that is fine — the state is what we
            // want. Any other filesystem error aborts this blob without touching Postgres.
            let path = blob_store::blob_path(&self.upload_dir, &locked.content_hash);
            let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    txn.rollback().await?;
                    return Err(DbErr::Custom(format!(
                        "gc: byte-delete of {} failed: {e}",
                        locked.content_hash
                    )));
                }
            }
            blob_gc::Entity::delete_by_id(locked.content_hash.clone())
                .exec(&txn)
                .await?;
            txn.commit().await?;
            report.swept += 1;
            report.swept_bytes = report.swept_bytes.saturating_add(bytes);
            info!(
                hash = %locked.content_hash,
                bytes,
                marked = %marked,
                swept_at = %now,
                "gc: byte-deleted collectable blob past its grace window"
            );
        }
        Ok(())
    }

    /// The set of content addresses physically present in the blob store (`blobs/{hash}.bin`).
    /// The blob store — not Postgres — is the source of truth for what bytes exist, so the mark
    /// pass reclaims orphans Postgres has no row for. A missing directory is an empty store.
    fn present_blob_hashes(&self) -> std::io::Result<BTreeSet<String>> {
        let dir = blob_store::blobs_dir(&self.upload_dir);
        let mut out = BTreeSet::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(hash) = name.strip_suffix(".bin")
                && is_hex64(hash)
            {
                out.insert(hash.to_string());
            }
        }
        Ok(out)
    }
}

/// Whether `s` is a 64-char lowercase-hex string (a SHA-256 content address). Guards the
/// blob-store scan against stray files that are not content-addressed blobs.
fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex64_accepts_a_content_address_and_rejects_others() {
        assert!(is_hex64(&"a".repeat(64)));
        assert!(is_hex64(&"0123456789abcdef".repeat(4)));
        assert!(!is_hex64(&"a".repeat(63)), "too short");
        assert!(
            !is_hex64(&"A".repeat(64)),
            "uppercase is not our canonical hex"
        );
        assert!(!is_hex64(&"g".repeat(64)), "non-hex char");
        assert!(!is_hex64("incoming"), "not a blob name");
    }
}
