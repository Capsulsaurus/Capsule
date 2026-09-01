//! The server-side integrity scrub (slice `S-C14`) — the **read-only** verifier of a
//! frozen (or live-quiesced) Postgres index against its content-addressed blob store.
//!
//! PostgreSQL is the authoritative index and the blob store holds the ciphertext bytes plus
//! the manifest-envelope objects; *authoritative* says which copy wins, **not** that an
//! implementation bug cannot let the two drift. This scrub is the external code path that
//! proves they agree — deliberately separate from the write path, so a hot-path bug cannot
//! also be a bug in the check that would catch it.
//!
//! It is **read-only by design**: it classifies and reports, and repairs *nothing*. Every
//! query below is a `SELECT`; there is no `ActiveModel`, no `INSERT`/`UPDATE`/`DELETE`, and no
//! filesystem write anywhere in this module. Repair stays with the paths that own it — the
//! [reference-count GC][gc] for orphans, the index rebuild for a lost index, operator action
//! for quarantines — so the scrub can never itself become the deletion bug it exists to catch.
//!
//! ## What it validates (the maintenance-doc checks, on the landed schema)
//!
//! 1. **Row → blob presence** ([`FindingClass::DanglingReference`]). Every committed
//!    blob-referencing row (feed `blobs`, `assets.file_hash`, `quota_ledger`) resolves to a
//!    file at `blobs/{hash}.bin`. A miss is a *dangling reference* — never auto-resolved —
//!    **except** a missing *original* on an `awaiting-original` asset (the staged-upload
//!    carve-out: [`FeedEntryInput::original_held`] `= false`), which is expected.
//! 2. **Blob → row presence** ([`FindingClass::OrphanBlob`]). Every file under `blobs/` is
//!    referenced by at least one committed row. A miss is an *orphan*, reported for the GC
//!    path — never removed by the scrub.
//! 3. **Byte integrity, deep mode** ([`FindingClass::CorruptBlob`]). Blob bytes re-hash to
//!    their content-addressed name — the server-side bit-rot check. Heavy I/O, so gated on
//!    `deep` and streamed per file with bounded memory.
//! 4. **Envelope chain ⇄ index agreement** ([`FindingClass::ChainBreak`]). The append-only
//!    custody-receipt log — the server's materialized, envelope-derived provenance chain —
//!    walks forward per `server_id`: gap-free `receipt_seq` from a `prior_receipt_hash = NULL`
//!    genesis, each link's `prior_receipt_hash` matching its predecessor's `receipt_hash`.
//!    Truncating the sequence (a deleted mid-chain receipt) breaks the walk.
//! 5. **Mirrored-fact agreement** ([`FindingClass::MirroredFactMismatch`]). The declared
//!    ciphertext **size** a server mirrors out of the envelope must agree across its copies:
//!    the feed `blobs[].size`, the `custody_receipts.size`, and the physical blob length.
//! 6. **Debris + quarantine inventory** ([`FindingClass::IncomingDebris`],
//!    [`FindingClass::Quarantine`]). Stale `{upload_id}.bin` staging files with no live
//!    session, and every quarantined blob, are enumerated so debris and unresolved forensics
//!    cannot silently accumulate.
//!
//! A discrepancy is **classified, never adjudicated**: a [`Finding`] names the failed check
//! and both sides' evidence and deliberately does not assume whether the index or the blob
//! store is at fault. Every finding is logged structured (per the traceability principle); the
//! run emits per-class counts (zero on a clean store), and a non-zero finding count is the
//! exit signal operators alert on.
//!
//! SSoT: [Maintenance — Server-Side Integrity Scrub][doc].
//!
//! [doc]: ../../../../capsule-docs/src/content/docs/design/filesystem/maintenance.md
//! [gc]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md
//! [`FeedEntryInput::original_held`]: crate::sync::FeedEntryInput::original_held

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use ::entity::{asset, blob_gc, custody_receipt, quota_ledger, sync_entry};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};
use tracing::{debug, info, instrument, warn};

use crate::blob_store::{self, is_content_hash};
use crate::sync::FeedBlobManifest;

/// The class of an integrity discrepancy — one variant per maintenance-doc check. The
/// declaration order is the report's stable sort key (a derived `Ord`), so two runs over an
/// unchanged store emit byte-identical reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingClass {
    /// Check 1: a committed row references a blob missing from the store (not an
    /// `awaiting-original` original).
    DanglingReference,
    /// Check 2: a blob file is referenced by no committed row.
    OrphanBlob,
    /// Check 3 (deep): a blob's bytes do not re-hash to its content-addressed name.
    CorruptBlob,
    /// Check 4: the custody-receipt chain is not a gap-free forward walk.
    ChainBreak,
    /// Check 5: a declared size disagrees across the copies that mirror it.
    MirroredFactMismatch,
    /// Check 6: a stale `{upload_id}.bin` staging file with no live session.
    IncomingDebris,
    /// Check 6: a quarantined blob, enumerated for operator action.
    Quarantine,
}

impl FindingClass {
    /// A stable snake_case tag for structured logs and report keys.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::DanglingReference => "dangling_reference",
            Self::OrphanBlob => "orphan_blob",
            Self::CorruptBlob => "corrupt_blob",
            Self::ChainBreak => "chain_break",
            Self::MirroredFactMismatch => "mirrored_fact_mismatch",
            Self::IncomingDebris => "incoming_debris",
            Self::Quarantine => "quarantine",
        }
    }

    /// Every class, in report order — so a clean-store report can carry an explicit zero for
    /// each (per-class counts are zero on a clean store, never merely absent).
    #[must_use]
    pub const fn all() -> [FindingClass; 7] {
        [
            Self::DanglingReference,
            Self::OrphanBlob,
            Self::CorruptBlob,
            Self::ChainBreak,
            Self::MirroredFactMismatch,
            Self::IncomingDebris,
            Self::Quarantine,
        ]
    }
}

/// One classified integrity discrepancy: the failed check, the subject, and both sides'
/// evidence. Deliberately does not adjudicate fault (misassigned fault is how a "repair"
/// deletes the last good copy).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// Which check failed.
    pub class: FindingClass,
    /// The content address at fault, when the finding is blob-scoped.
    pub content_hash: Option<String>,
    /// The asset the finding concerns, when known.
    pub asset_id: Option<String>,
    /// Human-readable evidence for the operator (structured log detail, not a user string).
    pub detail: String,
}

/// A structured summary of one scrub pass. Deterministic: `findings` is sorted, so two runs
/// over an unchanged store produce equal reports (the idempotency contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubReport {
    /// Every classified discrepancy, sorted for a stable report.
    pub findings: Vec<Finding>,
    /// Per-class counts, carrying an explicit zero for every class.
    pub counts: BTreeMap<&'static str, usize>,
    /// Blob files scanned in the content-addressed store.
    pub scanned_blobs: usize,
    /// Distinct committed content addresses referenced by the index.
    pub scanned_references: usize,
    /// Whether the deep byte-integrity re-hash ran this pass.
    pub deep: bool,
}

impl ScrubReport {
    /// The total number of findings — the operator's alert signal. Zero on a clean store.
    #[must_use]
    pub fn total(&self) -> usize {
        self.findings.len()
    }

    /// Whether the store is clean (no findings). The binary maps `!is_clean()` to a non-zero
    /// process exit.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// The count of findings of one class.
    #[must_use]
    pub fn count(&self, class: FindingClass) -> usize {
        self.counts.get(class.tag()).copied().unwrap_or(0)
    }
}

/// One committed reference to a content address, with the evidence the presence + mirror
/// checks reason over.
#[derive(Debug, Clone)]
struct BlobRef {
    asset_id: Option<String>,
    role: String,
    /// The size this reference declares for the blob, when it carries one.
    declared_size: Option<u64>,
    /// Where the reference came from (for the finding evidence).
    source: &'static str,
}

/// The operator-invokable, read-only integrity scrub over one content-addressed blob store.
///
/// Construct with [`IntegrityScrub::new`]; a binary crons [`IntegrityScrub::run`]. There is
/// no scheduling framework and no clock seam — the scrub does no time-based policy, it only
/// compares the two stores as they stand.
pub struct IntegrityScrub {
    upload_dir: PathBuf,
}

impl IntegrityScrub {
    /// A scrub over `upload_dir`'s blob store (`{upload_dir}/blobs/{hash}.bin`) and staging
    /// area (`{upload_dir}/{upload_id}.bin`).
    #[must_use]
    pub fn new(upload_dir: PathBuf) -> Self {
        Self { upload_dir }
    }

    /// Run one full scrub pass and return the classified [`ScrubReport`]. With `deep`, also
    /// re-hashes every blob's bytes (check 3). **Reads only** — issues no write of any kind.
    #[instrument(skip(self, db), fields(upload_dir = %self.upload_dir.display(), deep))]
    pub async fn run<C: ConnectionTrait>(&self, db: &C, deep: bool) -> Result<ScrubReport, DbErr> {
        info!(deep, "scrub: integrity pass starting (read-only)");

        // ── Gather both sides ────────────────────────────────────────────────────────────
        let present = self
            .present_blobs()
            .map_err(|e| DbErr::Custom(format!("scrub: scan blob store: {e}")))?;
        let (referenced, original_held) = self.index_references(db).await?;

        let mut findings: Vec<Finding> = Vec::new();

        // ── Check 1: row → blob presence (with the awaiting-original carve-out) ───────────
        self.check_row_to_blob(&referenced, &original_held, &present, &mut findings);
        // ── Check 2: blob → row presence ─────────────────────────────────────────────────
        Self::check_blob_to_row(&referenced, &present, &mut findings);
        // ── Check 3: deep byte integrity ─────────────────────────────────────────────────
        if deep {
            self.check_deep(&present, &mut findings)?;
        }
        // ── Check 4: custody-receipt chain agreement ─────────────────────────────────────
        Self::check_chain(db, &mut findings).await?;
        // ── Check 5: mirrored-fact (declared size) agreement ─────────────────────────────
        Self::check_mirrored_sizes(db, &referenced, &present, &mut findings).await?;
        // ── Check 6: debris + quarantine inventory ───────────────────────────────────────
        self.check_incoming_debris(&mut findings)
            .map_err(|e| DbErr::Custom(format!("scrub: scan staging area: {e}")))?;
        Self::check_quarantine(db, &mut findings).await?;

        // Deterministic report: sort so two runs over an unchanged store are byte-identical.
        findings.sort();
        for f in &findings {
            warn!(
                class = f.class.tag(),
                content_hash = f.content_hash.as_deref().unwrap_or(""),
                asset_id = f.asset_id.as_deref().unwrap_or(""),
                detail = %f.detail,
                "scrub: integrity finding"
            );
        }

        let mut counts: BTreeMap<&'static str, usize> =
            FindingClass::all().iter().map(|c| (c.tag(), 0)).collect();
        for f in &findings {
            *counts.get_mut(f.class.tag()).expect("class tag pre-seeded") += 1;
        }

        let report = ScrubReport {
            scanned_blobs: present.len(),
            scanned_references: referenced.len(),
            deep,
            findings,
            counts,
        };
        info!(
            total = report.total(),
            scanned_blobs = report.scanned_blobs,
            scanned_references = report.scanned_references,
            deep,
            dangling_reference = report.count(FindingClass::DanglingReference),
            orphan_blob = report.count(FindingClass::OrphanBlob),
            corrupt_blob = report.count(FindingClass::CorruptBlob),
            chain_break = report.count(FindingClass::ChainBreak),
            mirrored_fact_mismatch = report.count(FindingClass::MirroredFactMismatch),
            incoming_debris = report.count(FindingClass::IncomingDebris),
            quarantine = report.count(FindingClass::Quarantine),
            "scrub: integrity pass complete"
        );
        Ok(report)
    }

    // ─────────────────────────────── the index side ────────────────────────────────────

    /// Build the committed blob-reference index and the per-asset `original_held` carve-out
    /// map. References come from the three committed sources the write paths maintain: the
    /// sync feed's `blobs` (originals + derivatives — the S-C3 `indexed` SSoT), `assets`
    /// (originals), and `quota_ledger` (auxiliary blobs). Never a separately-drifting counter.
    async fn index_references<C: ConnectionTrait>(
        &self,
        db: &C,
    ) -> Result<(BTreeMap<String, Vec<BlobRef>>, HashMap<String, bool>), DbErr> {
        let mut referenced: BTreeMap<String, Vec<BlobRef>> = BTreeMap::new();
        let mut original_held: HashMap<String, bool> = HashMap::new();

        // Feed references, oldest→newest so the newest `original_held` wins per asset.
        let feed = sync_entry::Entity::find()
            .order_by_asc(sync_entry::Column::FeedSeq)
            .all(db)
            .await?;
        for row in feed {
            original_held.insert(row.asset_id.clone(), row.original_held);
            let manifest: FeedBlobManifest = serde_json::from_value(row.blobs)
                .map_err(|e| DbErr::Custom(format!("scrub: decode feed blobs: {e}")))?;
            if let Some(original) = manifest.original {
                referenced
                    .entry(original.ciphertext_hash)
                    .or_default()
                    .push(BlobRef {
                        asset_id: Some(row.asset_id.clone()),
                        role: original.role,
                        declared_size: Some(original.size),
                        source: "feed",
                    });
            }
            for d in manifest.derivatives {
                referenced
                    .entry(d.ciphertext_hash)
                    .or_default()
                    .push(BlobRef {
                        asset_id: Some(row.asset_id.clone()),
                        role: d.role,
                        declared_size: Some(d.size),
                        source: "feed",
                    });
            }
        }

        // `assets` originals — committed rows only (`uploaded = true`).
        let assets = asset::Entity::find()
            .filter(asset::Column::Uploaded.eq(true))
            .all(db)
            .await?;
        for a in assets {
            let size = u64::try_from(a.file_size).ok();
            referenced.entry(a.file_hash).or_default().push(BlobRef {
                asset_id: Some(a.id.clone()),
                role: "original".to_string(),
                declared_size: size,
                source: "asset",
            });
            // The feed's derived `original_held` takes precedence; an `assets`-only asset
            // falls back to its `uploaded` flag (a committed upload holds its original).
            original_held.entry(a.id).or_insert(a.uploaded);
        }

        // Auxiliary blobs held in the quota ledger (refcount > 0).
        let ledger = quota_ledger::Entity::find()
            .filter(quota_ledger::Column::Refcount.gt(0))
            .all(db)
            .await?;
        for l in ledger {
            let size = u64::try_from(l.byte_size).ok();
            referenced.entry(l.content_hash).or_default().push(BlobRef {
                asset_id: None,
                role: l.blob_kind,
                declared_size: size,
                source: "quota_ledger",
            });
        }

        Ok((referenced, original_held))
    }

    // ────────────────────────────────── check 1 ────────────────────────────────────────

    /// Row → blob presence. A committed reference to a blob missing from the store is a
    /// dangling reference — one finding per missing hash — **unless** every reference to it is
    /// an original of an `awaiting-original` asset (the staged-upload carve-out).
    fn check_row_to_blob(
        &self,
        referenced: &BTreeMap<String, Vec<BlobRef>>,
        original_held: &HashMap<String, bool>,
        present: &BTreeMap<String, u64>,
        findings: &mut Vec<Finding>,
    ) {
        for (hash, refs) in referenced {
            if present.contains_key(hash) {
                continue;
            }
            // A reference is carve-out-eligible iff it is an `original` whose asset is
            // awaiting its original blob (`original_held = false`).
            let non_carveout: Vec<&BlobRef> = refs
                .iter()
                .filter(|r| {
                    let awaiting = r.role == "original"
                        && r.asset_id.as_deref().and_then(|a| original_held.get(a)) == Some(&false);
                    !awaiting
                })
                .collect();

            if non_carveout.is_empty() {
                debug!(
                    content_hash = %hash,
                    "scrub: missing original on awaiting-original asset — expected staged state, no finding"
                );
                continue;
            }

            let evidence = non_carveout
                .iter()
                .map(|r| {
                    format!(
                        "{}:{}({})",
                        r.source,
                        r.role,
                        r.asset_id.as_deref().unwrap_or("-")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let asset_id = non_carveout.iter().find_map(|r| r.asset_id.clone());
            findings.push(Finding {
                class: FindingClass::DanglingReference,
                content_hash: Some(hash.clone()),
                asset_id,
                detail: format!(
                    "committed row(s) reference blob {hash} but no file at blobs/{hash}.bin; referenced by [{evidence}]"
                ),
            });
        }
    }

    // ────────────────────────────────── check 2 ────────────────────────────────────────

    /// Blob → row presence. A file under `blobs/` referenced by no committed row is an orphan.
    fn check_blob_to_row(
        referenced: &BTreeMap<String, Vec<BlobRef>>,
        present: &BTreeMap<String, u64>,
        findings: &mut Vec<Finding>,
    ) {
        for (hash, size) in present {
            if referenced.contains_key(hash) {
                continue;
            }
            findings.push(Finding {
                class: FindingClass::OrphanBlob,
                content_hash: Some(hash.clone()),
                asset_id: None,
                detail: format!(
                    "blob {hash} ({size} bytes) is present in the store but referenced by no committed row"
                ),
            });
        }
    }

    // ────────────────────────────────── check 3 ────────────────────────────────────────

    /// Deep byte integrity: every present blob's bytes must re-hash to its content-addressed
    /// name. Streamed per file with bounded memory (`get_file_hash`).
    fn check_deep(
        &self,
        present: &BTreeMap<String, u64>,
        findings: &mut Vec<Finding>,
    ) -> Result<(), DbErr> {
        for hash in present.keys() {
            let path = blob_store::blob_path(&self.upload_dir, hash);
            let computed = capsule_core::utils::hash::get_file_hash(&path)
                .map_err(|e| DbErr::Custom(format!("scrub: re-hash {hash}: {e}")))?;
            if &computed != hash {
                findings.push(Finding {
                    class: FindingClass::CorruptBlob,
                    content_hash: Some(hash.clone()),
                    asset_id: None,
                    detail: format!(
                        "blob at blobs/{hash}.bin re-hashes to {computed}; content-addressed name and bytes disagree (bit rot)"
                    ),
                });
            }
        }
        Ok(())
    }

    // ────────────────────────────────── check 4 ────────────────────────────────────────

    /// Custody-receipt chain agreement. Per `server_id`, the receipt log must be a gap-free
    /// forward walk: the genesis (`receipt_seq = 1`) carries `prior_receipt_hash = NULL`, and
    /// every later link's `prior_receipt_hash` equals its predecessor's `receipt_hash` with a
    /// contiguous `receipt_seq`. A truncation (deleted mid-chain receipt) breaks the walk.
    async fn check_chain<C: ConnectionTrait>(
        db: &C,
        findings: &mut Vec<Finding>,
    ) -> Result<(), DbErr> {
        let receipts = custody_receipt::Entity::find()
            .order_by_asc(custody_receipt::Column::ServerId)
            .order_by_asc(custody_receipt::Column::ReceiptSeq)
            .all(db)
            .await?;

        // Group by server_id, preserving the seq-ascending order.
        let mut by_server: BTreeMap<String, Vec<custody_receipt::Model>> = BTreeMap::new();
        for r in receipts {
            by_server.entry(r.server_id.clone()).or_default().push(r);
        }

        for (server_id, chain) in by_server {
            let mut prev: Option<&custody_receipt::Model> = None;
            for row in &chain {
                match prev {
                    None => {
                        // Genesis link.
                        if row.receipt_seq != 1 {
                            findings.push(Finding {
                                class: FindingClass::ChainBreak,
                                content_hash: None,
                                asset_id: Some(row.asset_id.clone()),
                                detail: format!(
                                    "custody chain for server {server_id} starts at receipt_seq {} (expected 1); earlier receipts truncated",
                                    row.receipt_seq
                                ),
                            });
                        }
                        if row.prior_receipt_hash.is_some() {
                            findings.push(Finding {
                                class: FindingClass::ChainBreak,
                                content_hash: None,
                                asset_id: Some(row.asset_id.clone()),
                                detail: format!(
                                    "custody chain genesis for server {server_id} (seq {}) carries a prior_receipt_hash but should be NULL",
                                    row.receipt_seq
                                ),
                            });
                        }
                    }
                    Some(p) => {
                        let seq_ok = row.receipt_seq == p.receipt_seq + 1;
                        let link_ok =
                            row.prior_receipt_hash.as_deref() == Some(p.receipt_hash.as_str());
                        if !seq_ok || !link_ok {
                            findings.push(Finding {
                                class: FindingClass::ChainBreak,
                                content_hash: None,
                                asset_id: Some(row.asset_id.clone()),
                                detail: format!(
                                    "custody chain break for server {server_id} between receipt_seq {} and {}: {}{}",
                                    p.receipt_seq,
                                    row.receipt_seq,
                                    if seq_ok { "" } else { "non-contiguous seq; " },
                                    if link_ok {
                                        String::new()
                                    } else {
                                        format!(
                                            "prior_receipt_hash {:?} != predecessor receipt_hash {}",
                                            row.prior_receipt_hash, p.receipt_hash
                                        )
                                    }
                                ),
                            });
                        }
                    }
                }
                prev = Some(row);
            }
        }
        Ok(())
    }

    // ────────────────────────────────── check 5 ────────────────────────────────────────

    /// Mirrored-fact agreement over the declared ciphertext **size**. The size lives in up to
    /// three copies that must agree: the feed `blobs[].size`, the `custody_receipts.size`, and
    /// the physical blob length. A disagreement is one finding per copy-pair per `(asset,
    /// hash)`.
    async fn check_mirrored_sizes<C: ConnectionTrait>(
        db: &C,
        referenced: &BTreeMap<String, Vec<BlobRef>>,
        present: &BTreeMap<String, u64>,
        findings: &mut Vec<Finding>,
    ) -> Result<(), DbErr> {
        // Receipt-declared sizes keyed by (asset_id, ciphertext_hash).
        let receipts = custody_receipt::Entity::find().all(db).await?;
        let mut receipt_sizes: HashMap<(String, String), i64> = HashMap::new();
        for r in receipts {
            receipt_sizes.insert((r.asset_id, r.ciphertext_hash), r.size);
        }

        let mut seen: BTreeSet<(Option<String>, String)> = BTreeSet::new();
        for (hash, refs) in referenced {
            for r in refs {
                let Some(declared) = r.declared_size else {
                    continue;
                };
                // De-dup per (asset, hash) so a repeated reference is checked once.
                let key = (r.asset_id.clone(), hash.clone());
                if !seen.insert(key) {
                    continue;
                }

                // Feed/asset-declared size vs the physical blob length.
                if let Some(&physical) = present.get(hash)
                    && declared != physical
                {
                    findings.push(Finding {
                        class: FindingClass::MirroredFactMismatch,
                        content_hash: Some(hash.clone()),
                        asset_id: r.asset_id.clone(),
                        detail: format!(
                            "declared size {declared} ({} row) disagrees with physical blob length {physical} for {hash}",
                            r.source
                        ),
                    });
                }

                // Feed/asset-declared size vs the custody-receipt-declared size.
                if let Some(asset_id) = &r.asset_id
                    && let Some(&receipt_size) =
                        receipt_sizes.get(&(asset_id.clone(), hash.clone()))
                    && u64::try_from(receipt_size).ok() != Some(declared)
                {
                    findings.push(Finding {
                        class: FindingClass::MirroredFactMismatch,
                        content_hash: Some(hash.clone()),
                        asset_id: r.asset_id.clone(),
                        detail: format!(
                            "declared size {declared} ({} row) disagrees with custody_receipt size {receipt_size} for asset {asset_id} blob {hash}",
                            r.source
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    // ────────────────────────────────── check 6 ────────────────────────────────────────

    /// Debris inventory: `{upload_id}.bin` staging files directly under `upload_dir` (never
    /// under `blobs/`). On a quiesced store these have no live session and are debris; the
    /// scrub inventories them, the S-C1 startup scrub / discard machinery removes them.
    fn check_incoming_debris(&self, findings: &mut Vec<Finding>) -> std::io::Result<()> {
        let entries = match std::fs::read_dir(&self.upload_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue; // the `blobs/` subdir and any `quarantine/` subdir are not debris
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name.strip_suffix(".bin") {
                findings.push(Finding {
                    class: FindingClass::IncomingDebris,
                    content_hash: None,
                    asset_id: None,
                    detail: format!(
                        "stale staging file {id}.bin under upload_dir with no live session (debris)"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Quarantine inventory: every blob the GC path has flagged `quarantined` — enumerated so
    /// unresolved forensics cannot silently accumulate. The landed schema flags in `blob_gc`
    /// rather than moving bytes to a `quarantine/` directory.
    async fn check_quarantine<C: ConnectionTrait>(
        db: &C,
        findings: &mut Vec<Finding>,
    ) -> Result<(), DbErr> {
        let rows = blob_gc::Entity::find()
            .filter(blob_gc::Column::Quarantined.eq(true))
            .all(db)
            .await?;
        for row in rows {
            findings.push(Finding {
                class: FindingClass::Quarantine,
                content_hash: Some(row.content_hash.clone()),
                asset_id: None,
                detail: format!(
                    "blob {} is quarantined (integrity fault) — awaiting operator action",
                    row.content_hash
                ),
            });
        }
        Ok(())
    }

    // ─────────────────────────────── the blob-store side ───────────────────────────────

    /// Every content address physically present in `blobs/`, mapped to its file length. The
    /// blob store — not Postgres — is the source of truth for what bytes exist. A missing
    /// directory is an empty store. Non-content-addressed names are ignored.
    fn present_blobs(&self) -> std::io::Result<BTreeMap<String, u64>> {
        let dir = blob_store::blobs_dir(&self.upload_dir);
        let mut out = BTreeMap::new();
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
                && is_content_hash(hash)
            {
                let len = entry.metadata()?.len();
                out.insert(hash.to_string(), len);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn href(role: &str, asset: Option<&str>, size: Option<u64>, source: &'static str) -> BlobRef {
        BlobRef {
            asset_id: asset.map(str::to_string),
            role: role.to_string(),
            declared_size: size,
            source,
        }
    }

    #[test]
    fn finding_class_tags_are_stable_and_total() {
        assert_eq!(FindingClass::all().len(), 7);
        assert_eq!(FindingClass::DanglingReference.tag(), "dangling_reference");
        assert_eq!(FindingClass::Quarantine.tag(), "quarantine");
    }

    #[test]
    fn dangling_reference_fires_unless_awaiting_original() {
        let hash = "a".repeat(64);
        let mut referenced: BTreeMap<String, Vec<BlobRef>> = BTreeMap::new();
        referenced.insert(
            hash.clone(),
            vec![href("original", Some("asset-1"), Some(10), "feed")],
        );
        let present: BTreeMap<String, u64> = BTreeMap::new();
        let scrub = IntegrityScrub::new(PathBuf::from("/tmp/does-not-matter"));

        // original_held = true -> a missing original is a dangling reference.
        let mut held = HashMap::new();
        held.insert("asset-1".to_string(), true);
        let mut findings = Vec::new();
        scrub.check_row_to_blob(&referenced, &held, &present, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].class, FindingClass::DanglingReference);

        // original_held = false -> the carve-out suppresses the finding.
        let mut held = HashMap::new();
        held.insert("asset-1".to_string(), false);
        let mut findings = Vec::new();
        scrub.check_row_to_blob(&referenced, &held, &present, &mut findings);
        assert!(
            findings.is_empty(),
            "awaiting-original original is carved out"
        );
    }

    #[test]
    fn orphan_fires_only_for_unreferenced_present_blobs() {
        let referenced_hash = "b".repeat(64);
        let orphan_hash = "c".repeat(64);
        let mut referenced: BTreeMap<String, Vec<BlobRef>> = BTreeMap::new();
        referenced.insert(
            referenced_hash.clone(),
            vec![href("original", Some("asset-1"), Some(1), "feed")],
        );
        let mut present: BTreeMap<String, u64> = BTreeMap::new();
        present.insert(referenced_hash, 1);
        present.insert(orphan_hash.clone(), 2);

        let mut findings = Vec::new();
        IntegrityScrub::check_blob_to_row(&referenced, &present, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].class, FindingClass::OrphanBlob);
        assert_eq!(
            findings[0].content_hash.as_deref(),
            Some(orphan_hash.as_str())
        );
    }

    #[test]
    fn findings_sort_is_stable_by_class_then_hash() {
        let mut findings = vec![
            Finding {
                class: FindingClass::OrphanBlob,
                content_hash: Some("z".repeat(64)),
                asset_id: None,
                detail: "b".into(),
            },
            Finding {
                class: FindingClass::DanglingReference,
                content_hash: Some("a".repeat(64)),
                asset_id: None,
                detail: "a".into(),
            },
        ];
        findings.sort();
        assert_eq!(findings[0].class, FindingClass::DanglingReference);
        assert_eq!(findings[1].class, FindingClass::OrphanBlob);
    }
}
