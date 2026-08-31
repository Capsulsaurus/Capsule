//! The server-side integrity scrub (`S-C14`) — read-only, classifying, never repairing.
//!
//! # Why it may not fix anything
//!
//! The scrub exists to catch the case where the index and the blob store have drifted, and the
//! only honest thing it can do about a discrepancy is **name it**. It does not adjudicate which
//! side is wrong, because misassigned fault is how a "repair" deletes the last good copy: a
//! dangling reference looks identical whether the blob was lost or the row was written in
//! error, and the two call for opposite actions.
//!
//! Repair therefore stays with the paths that own it — [`crate::gc`] for orphans, an index
//! rebuild for a lost index, an operator for quarantines. Keeping the check separate from every
//! write path is also what stops a hot-path bug from being a bug in the check that would catch
//! it, which is the whole reason this is a distinct code path rather than an assertion.
//!
//! # What it checks, and what it cannot
//!
//! | Check | Status |
//! | --- | --- |
//! | Row → blob presence, with the `awaiting-original` carve-out | done |
//! | Blob → row presence (orphans) | done |
//! | Byte integrity, re-hashing under [`Depth::Deep`] | done |
//! | Chain head resolves to a stored provenance blob | **partial** — see below |
//! | Envelope chain ⇄ index agreement | not possible; see below |
//! | Mirrored-fact agreement | not possible; see below |
//! | Debris and quarantine inventory | done |
//!
//! The last three all founder on the same thing, and it is not an oversight: **this server does
//! not parse signed CBOR.** The full chain walk and the mirrored-fact comparison both require
//! reading the manifest inside the provenance blob and comparing it against the projection the
//! index holds, and a key-free server that started decoding signed structures to check itself
//! would have taken on exactly the parsing surface `S-C30` exists to avoid. What *is* checkable
//! without decoding anything is that the chain head names a provenance blob the store actually
//! holds — a strictly weaker claim, reported as its own class so nobody mistakes it for the
//! agreement check. The rest is `S-C45`.
//!
//! # The report is the product
//!
//! Per-class counts, every finding carrying both sides' evidence, and zero findings on a clean
//! store. An operator alerts on the count; a human reads the findings. A scrub that returned a
//! boolean would be a scrub nobody could act on.

use std::collections::BTreeSet;
use std::sync::Arc;

use capsule_core::crypto::hash::Sha256Hasher;

use crate::blob::{BlobStore, ContentAddress};
use crate::index::{AssetIndex, AssetState};
use crate::store::{AssetId, BlobRole, StoreError, UploadSessionStore};

/// How many bytes to hash at a time when re-hashing a blob.
const HASH_WINDOW: usize = 1024 * 1024;

/// How much work a pass may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Structural only: presence, orphans, inventory. Cheap enough to run often.
    Structural,
    /// Also re-hash every blob's bytes — the bit-rot check.
    ///
    /// Heavy I/O by definition, so it carries a byte budget rather than running to completion:
    /// a scrub that saturates the disk is a scrub an operator turns off.
    Deep {
        /// The most bytes this pass will read. Blobs past it are left for the next run.
        budget: u64,
    },
}

/// One thing the scrub found.
///
/// Every variant carries both sides' evidence, because a report that says "inconsistent" tells
/// an operator to go looking rather than telling them where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// A committed row references a blob the store does not hold.
    ///
    /// The loud integrity error. Never auto-resolved: erasing the row would destroy the only
    /// record that the asset should exist.
    DanglingReference {
        /// The asset that references it.
        asset: AssetId,
        /// The role it holds the address under.
        role: BlobRole,
        /// The address that is missing.
        address: ContentAddress,
    },
    /// A blob under `blobs/` that no committed row references.
    ///
    /// Reported for the collector, never removed here — see [`crate::gc`].
    Orphan {
        /// The unreferenced address.
        address: ContentAddress,
    },
    /// A blob's bytes do not hash to its own name. Bit rot.
    ByteMismatch {
        /// The address the blob is stored under.
        address: ContentAddress,
        /// What its bytes actually hash to.
        found: String,
    },
    /// An asset's chain head names a provenance blob the store does not hold.
    ///
    /// The weaker half of the envelope-chain check — see the module docs and `S-C45`.
    ChainHeadUnresolvable {
        /// The asset.
        asset: AssetId,
        /// The provenance address its chain head should resolve to, if it holds one at all.
        provenance: Option<ContentAddress>,
    },
    /// Something under the blob root that is not a blob.
    Debris {
        /// Its path, relative to the store root.
        path: String,
    },
    /// A blob held out of the store for an operator.
    ///
    /// Not a fault the scrub found — it is a fault somebody already found — but unresolved
    /// forensics that accumulate silently are their own problem.
    Quarantined {
        /// The address being held.
        address: ContentAddress,
        /// The `error.*` code naming what failed.
        code: String,
    },
    /// An asset's provenance blob is not a manifest this server can read (`S-C45`).
    ///
    /// Reported rather than treated as agreement. The provenance blob is deliberately
    /// server-visible signed CBOR — it carries no plaintext secrets by construction — so a blob
    /// under that role that does not decode is the store holding something that is not what the
    /// index says it is.
    ManifestUnreadable {
        /// The asset.
        asset: AssetId,
        /// The address the row points at.
        address: ContentAddress,
    },
    /// A fact Postgres mirrors out of the signed manifest disagrees with the manifest (`S-C45`).
    ///
    /// Check 5. Both sides' values, because a report saying "they disagree" tells an operator to
    /// go looking rather than telling them what to look at — and the scrub deliberately does not
    /// adjudicate which side is wrong.
    MirroredFactMismatch {
        /// The asset.
        asset: AssetId,
        /// Which mirrored fact.
        fact: &'static str,
        /// What the index holds.
        index: String,
        /// What the signed manifest says.
        manifest: String,
    },
    /// A staged upload with no live session behind it.
    StaleStage {
        /// The upload the stage belongs to.
        upload: crate::store::UploadId,
    },
}

impl Finding {
    /// The class name an operator counts and alerts on.
    pub fn class(&self) -> &'static str {
        match self {
            Self::DanglingReference { .. } => "dangling_reference",
            Self::Orphan { .. } => "orphan",
            Self::ByteMismatch { .. } => "byte_mismatch",
            Self::ChainHeadUnresolvable { .. } => "chain_head_unresolvable",
            Self::ManifestUnreadable { .. } => "manifest_unreadable",
            Self::MirroredFactMismatch { .. } => "mirrored_fact_mismatch",
            Self::Debris { .. } => "debris",
            Self::Quarantined { .. } => "quarantined",
            Self::StaleStage { .. } => "stale_stage",
        }
    }
}

/// What one pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubReport {
    /// Every finding, in the order the pass produced them.
    pub findings: Vec<Finding>,
    /// How many bytes the deep check actually read.
    pub bytes_hashed: u64,
    /// Whether the deep check stopped early because it reached its budget.
    ///
    /// Reported rather than silent: a clean report from a truncated pass is not a clean store,
    /// and an operator alerting on the finding count needs to know the difference.
    pub budget_exhausted: bool,
}

impl ScrubReport {
    /// Whether the store and index agree.
    ///
    /// A **truncated** deep pass with no findings is not clean: it did not finish looking.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && !self.budget_exhausted
    }

    /// How many findings of `class` this pass produced.
    pub fn count(&self, class: &str) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.class() == class)
            .count()
    }

    /// The per-class counts, in class order.
    pub fn counts(&self) -> Vec<(&'static str, usize)> {
        let mut classes: Vec<&'static str> = self
            .findings
            .iter()
            .map(Finding::class)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        classes.sort_unstable();
        classes
            .into_iter()
            .map(|class| (class, self.count(class)))
            .collect()
    }
}

/// The scrub's collaborators.
#[derive(Debug, Clone)]
pub struct ScrubContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    uploads: Arc<dyn UploadSessionStore>,
}

impl ScrubContext {
    /// Assembles the scrub over the three stores it compares.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        uploads: Arc<dyn UploadSessionStore>,
    ) -> Self {
        Self {
            index,
            blobs,
            uploads,
        }
    }
}

/// Run one scrub pass.
///
/// Mutates nothing, by construction: every call below is a read.
///
/// # Errors
///
/// Propagates the first store failure. A pass that cannot read one of its two sides cannot
/// compare them, and reporting a partial comparison as a clean store would be the worst
/// possible answer.
#[tracing::instrument(skip(context), fields(depth = ?depth))]
pub async fn scrub(context: &ScrubContext, depth: Depth) -> Result<ScrubReport, StoreError> {
    let mut report = ScrubReport::default();

    rows_resolve_to_blobs(context, &mut report).await?;
    mirrored_facts_agree(context, &mut report).await?;
    blobs_are_referenced(context, depth, &mut report).await?;
    inventory(context, &mut report).await?;

    for (class, count) in report.counts() {
        tracing::warn!(class, count, "the integrity scrub found something");
    }
    tracing::info!(
        findings = report.findings.len(),
        bytes_hashed = report.bytes_hashed,
        clean = report.is_clean(),
        "an integrity scrub finished"
    );
    Ok(report)
}

/// Checks 1 and the weaker half of 4: every referenced address is held, and every asset's
/// chain head names a provenance blob that exists.
async fn rows_resolve_to_blobs(
    context: &ScrubContext,
    report: &mut ScrubReport,
) -> Result<(), StoreError> {
    let mut after: Option<AssetId> = None;
    loop {
        let rows = context.index.rows(after.as_ref(), 256).await?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            for blob in &row.blobs {
                let held = context
                    .blobs
                    .stat(&blob.address)
                    .await
                    .map_err(blob_failure)?
                    .is_some();
                if held {
                    continue;
                }
                // The carve-out: a missing original on an asset still awaiting one is expected
                // staged-upload state, not corruption. Note this port cannot actually produce
                // that combination — see `S-C40` — so the arm is here because the *contract*
                // has it and a future `S-C40` must not have to remember to add it.
                if blob.role == BlobRole::Original && !row.original_held() {
                    continue;
                }
                report.findings.push(Finding::DanglingReference {
                    asset: row.asset_id.clone(),
                    role: blob.role,
                    address: blob.address.clone(),
                });
            }

            // A published asset claims a chain head; it must name a provenance blob the store
            // holds. Pending rows are exempt: their bundle has not landed yet.
            if row.state != AssetState::Pending && row.chain_head.is_some() {
                let provenance = row.address_for(BlobRole::Provenance).cloned();
                let resolvable = match &provenance {
                    Some(address) => context
                        .blobs
                        .stat(address)
                        .await
                        .map_err(blob_failure)?
                        .is_some(),
                    None => false,
                };
                if !resolvable {
                    report.findings.push(Finding::ChainHeadUnresolvable {
                        asset: row.asset_id.clone(),
                        provenance,
                    });
                }
            }
        }
        after = rows.last().map(|row| row.asset_id.clone());
    }
}

/// Check 5: the facts the index mirrors out of the signed manifest still agree with it
/// (`S-C45`).
///
/// # The decision this slice needed, and the answer
///
/// *May the scrub decode signed CBOR?* Yes, and the question turns out to be easier than it
/// looked: the provenance blob is **deliberately server-visible** signed CBOR that carries no
/// plaintext secrets by construction (upload-protocol, *What Gets Uploaded*). Reading it is not a
/// privacy question at all. What is a real constraint is *how*: it decodes through
/// `capsule_core`'s own `AssetManifest`, never through a shape defined here, because a second
/// implementation of manifest parsing is exactly what sharing `capsule_core::validation` exists
/// to prevent.
///
/// It is also the one place where reading it is clearly *safe*: the scrub is read-only, off the
/// hot path, and acts on nothing it finds. A hot-path parser would be a second authority on what
/// a manifest says; this is a witness.
///
/// # What is compared, and what is not
///
/// Every fact the index mirrors and can therefore drift from: the asset and album the manifest
/// names, the suite and protocol it was written under, the epoch, the retention floor, and the
/// addresses the manifest commits to for the blobs the index points at. `client_version` and
/// `plaintext_size` are *in* the manifest and mirrored **nowhere**, so there is nothing to
/// compare them against — the maintenance doc lists them and this says plainly that it does not
/// check them rather than pretending to.
///
/// # Why check 4 is not here
///
/// The doc's envelope-chain check asks the scrub to walk the chain forward from `create`. This
/// server cannot: it holds **one** manifest per asset, not a sequence. A lifecycle write
/// re-points the provenance role, which leaves the superseded manifest unreferenced and
/// therefore collectable — so the chain the doc assumes is in the blob store is not. That is a
/// finding about the *design*, not about a store, and it is filed as `S-C52` rather than
/// approximated here.
async fn mirrored_facts_agree(
    context: &ScrubContext,
    report: &mut ScrubReport,
) -> Result<(), StoreError> {
    let mut after: Option<AssetId> = None;
    loop {
        let rows = context.index.rows(after.as_ref(), 256).await?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            // A pending row's bundle has not landed; there is nothing to disagree with yet.
            if row.state == AssetState::Pending {
                continue;
            }
            let Some(address) = row.address_for(BlobRole::Provenance).cloned() else {
                // Already reported as an unresolvable chain head by check 1's other half.
                continue;
            };
            let Some(stat) = context.blobs.stat(&address).await.map_err(blob_failure)? else {
                // Likewise: a missing provenance blob is a dangling reference, reported there.
                continue;
            };
            // A manifest is small by construction — it is signed CBOR carrying hashes, never
            // bytes — so it is read whole rather than windowed like the deep re-hash.
            let span = usize::try_from(stat.size).unwrap_or(usize::MAX);
            let Some(bytes) = context
                .blobs
                .read_at(&address, 0, span)
                .await
                .map_err(blob_failure)?
            else {
                continue;
            };
            let Ok(manifest) = capsule_core::cbor::from_slice::<
                capsule_core::crypto::provenance::manifest::AssetManifest,
            >(&bytes) else {
                report.findings.push(Finding::ManifestUnreadable {
                    asset: row.asset_id.clone(),
                    address,
                });
                continue;
            };
            compare(row, &manifest.core, report);
        }
        after = rows.last().map(|row| row.asset_id.clone());
    }
}

/// Every mirrored fact, compared field by field.
///
/// Split out so the list of what is checked reads as a list. Each entry is a pair of strings on
/// purpose: the report carries both sides' evidence and adjudicates neither, so rendering them
/// the same way is what keeps a finding readable without knowing the field's type.
fn compare(
    row: &crate::index::AssetRow,
    core: &capsule_core::crypto::provenance::manifest::ManifestCore,
    report: &mut ScrubReport,
) {
    let mut mismatch = |fact: &'static str, index: String, manifest: String| {
        if index != manifest {
            report.findings.push(Finding::MirroredFactMismatch {
                asset: row.asset_id.clone(),
                fact,
                index,
                manifest,
            });
        }
    };

    mismatch(
        "file_id",
        row.asset_id.as_str().to_owned(),
        core.file_id.to_string(),
    );
    mismatch(
        "album_id",
        row.album_id.as_str().to_owned(),
        core.album_id.to_string(),
    );
    mismatch(
        "crypto_suite_id",
        row.crypto_suite_id.to_string(),
        core.crypto_suite_id.to_string(),
    );
    mismatch(
        "protocol_version",
        row.protocol_version.clone(),
        core.protocol_version.clone(),
    );
    mismatch(
        "amk_version",
        row.amk_version.to_string(),
        core.amk_version.0.to_string(),
    );
    mismatch(
        "retention_until",
        row.retention_until
            .map(|at| at.to_string())
            .unwrap_or_default(),
        core.retention_until.clone().unwrap_or_default(),
    );

    // The addresses the manifest commits to, against the ones the index points at. Only where
    // the manifest carries them: the presence-by-action rule is core's, and an absent optional
    // is an absent claim rather than a claim of absence.
    if let Some(committed) = &core.metadata_blob_hash {
        mismatch(
            "metadata_blob_hash",
            row.address_for(BlobRole::Metadata)
                .map(|address| address.as_str().to_owned())
                .unwrap_or_default(),
            committed.to_hex(),
        );
    }
    if core.action.is_create()
        || core.action == capsule_core::crypto::provenance::action::Action::Replace
    {
        // The two actions that carry an original. A `create` whose original has not landed yet
        // is the staged-upload carve-out, and holds nothing to compare.
        if let Some(held) = row.address_for(BlobRole::Original) {
            mismatch(
                "ciphertext_hash",
                held.as_str().to_owned(),
                core.ciphertext_hash.to_hex(),
            );
        }
    }
}

/// Checks 2 and 3: orphans, and (deeply) bit rot.
async fn blobs_are_referenced(
    context: &ScrubContext,
    depth: Depth,
    report: &mut ScrubReport,
) -> Result<(), StoreError> {
    let budget = match depth {
        Depth::Structural => 0,
        Depth::Deep { budget } => budget,
    };

    let mut after = None;
    loop {
        let page = context
            .blobs
            .enumerate(after.as_ref(), 256)
            .await
            .map_err(blob_failure)?;
        for entry in &page.entries {
            if context.index.reference_count(&entry.address).await? == 0 {
                report.findings.push(Finding::Orphan {
                    address: entry.address.clone(),
                });
            }
            if matches!(depth, Depth::Deep { .. }) {
                if report.bytes_hashed.saturating_add(entry.size) > budget {
                    report.budget_exhausted = true;
                    continue;
                }
                if let Some(found) = rehash(context, &entry.address, entry.size).await? {
                    report.findings.push(Finding::ByteMismatch {
                        address: entry.address.clone(),
                        found,
                    });
                }
                report.bytes_hashed = report.bytes_hashed.saturating_add(entry.size);
            }
        }
        for path in &page.debris {
            report.findings.push(Finding::Debris { path: path.clone() });
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(()),
        }
    }
}

/// Re-hash `address`'s bytes window by window, returning what they actually hash to when that
/// disagrees with the name they are stored under.
async fn rehash(
    context: &ScrubContext,
    address: &ContentAddress,
    size: u64,
) -> Result<Option<String>, StoreError> {
    let mut hasher = Sha256Hasher::new();
    let mut offset = 0;
    while offset < size {
        let Some(window) = context
            .blobs
            .read_at(address, offset, HASH_WINDOW)
            .await
            .map_err(blob_failure)?
        else {
            // It vanished mid-pass. Not a byte mismatch — the row-side walk reports a missing
            // blob, and claiming rot for an absence would misclassify the finding.
            return Ok(None);
        };
        if window.is_empty() {
            break;
        }
        offset = offset.saturating_add(window.len() as u64);
        hasher.update(&window);
    }
    let found = hasher.finalize().to_hex();
    Ok((found != address.as_str()).then_some(found))
}

/// Check 6: what is held for an operator, and what is staged with nothing behind it.
async fn inventory(context: &ScrubContext, report: &mut ScrubReport) -> Result<(), StoreError> {
    for held in context.blobs.quarantined().await.map_err(blob_failure)? {
        report.findings.push(Finding::Quarantined {
            address: held.address,
            code: held.reason.code,
        });
    }

    for upload in context.blobs.staged().await.map_err(blob_failure)? {
        if context.uploads.read(&upload).await?.is_none() {
            report.findings.push(Finding::StaleStage { upload });
        }
    }
    Ok(())
}

/// Map a blob-store failure into the store error the scrub propagates.
fn blob_failure(error: crate::blob::BlobError) -> StoreError {
    StoreError::Unavailable {
        store: "blobs",
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
