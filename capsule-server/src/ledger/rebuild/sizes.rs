//! `capsule-server repair-sizes`: fill in the size of every blob reference the index recorded
//! at size `0`, from the length the blob store holds it at (#1128).
//!
//! Before lifecycle ops carried their blobs' sizes, every op re-pointed the asset's provenance
//! role — and a `metadata-update` or `replace` its metadata and original roles — at size `0`,
//! and the sync feed served that `0` to clients budgeting a fetch and to federation pullers
//! re-checking invariant 4. New ops record real sizes; this pass repairs the rows written
//! before that.
//!
//! # What it may change, and what it never does
//!
//! It is an index repair from the blob store, which is why it sits beside the rebuild rather
//! than in the scrub (which never writes) or the collector (which never touches a row's facts):
//!
//! - **Dry run by default.** It reports what it would repair and writes nothing until
//!   [`Mode::Apply`].
//! - It writes only through [`crate::index::AssetIndex::repair_blob_size`], which changes a reference's size
//!   **only** where it is `0` and the role and address still match. It never changes an address,
//!   a role, a non-zero size, or any other column, so a reference a live op re-pointed after
//!   the listing is left alone.
//! - A reference whose blob the store does not hold is reported and **kept**: the scrub owns
//!   the dangling reference, and a repair that dropped it would discard the evidence of a write
//!   the server accepted.
//! - A reference whose blob the store holds at zero bytes is reported and kept at `0`: that is
//!   the true size, and invariant 4 refusing it is a finding for an operator, not a number to
//!   invent.
//! - **Idempotent.** A second pass over a repaired index finds nothing to repair.

use super::{RebuildContext, RebuildError};
use crate::blob::ContentAddress;
use crate::gc::Mode;
use crate::index::SizeRepair;
use crate::store::{AssetId, BlobRole};

/// How many rows one index page holds.
const ROW_PAGE: usize = 256;

/// One blob reference the index recorded at size `0`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ZeroSized {
    /// The asset holding the reference.
    pub asset: AssetId,
    /// The role it holds.
    pub role: BlobRole,
    /// The address it names.
    pub address: ContentAddress,
}

/// A size-`0` reference and the size the store holds its blob at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Resized {
    /// The reference.
    pub reference: ZeroSized,
    /// The blob's length in the store.
    pub size: u64,
}

/// What one `repair-sizes` pass found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SizeRepairReport {
    /// Whether anything was written.
    pub applied: bool,
    /// References repaired under `--apply`, and what would be in a dry run.
    pub repaired: Vec<Resized>,
    /// References whose row changed between the listing and the write — re-pointed by a live
    /// op, or the asset gone — so the write matched nothing. Not a fault: the row moved on.
    pub moved: Vec<Resized>,
    /// References whose blob the store does not hold. Kept as they are.
    pub missing: Vec<ZeroSized>,
    /// References whose blob the store holds at zero bytes. Kept at `0`, its true size.
    pub empty: Vec<ZeroSized>,
}

impl SizeRepairReport {
    /// Whether anything here needs an operator: a reference this pass could not repair.
    pub fn has_findings(&self) -> bool {
        !self.missing.is_empty() || !self.empty.is_empty()
    }
}

/// Walk every index row and repair each size-`0` blob reference from the blob store.
///
/// # Errors
///
/// [`RebuildError`] when the index or the blob store cannot answer. A missing or empty blob is
/// a report line, not an error.
pub async fn repair_sizes(
    context: &RebuildContext,
    mode: Mode,
) -> Result<SizeRepairReport, RebuildError> {
    let mut report = SizeRepairReport {
        applied: mode == Mode::Apply,
        ..SizeRepairReport::default()
    };
    let mut after: Option<AssetId> = None;
    let mut scanned = 0_usize;
    loop {
        let rows = context.index.rows(after.as_ref(), ROW_PAGE).await?;
        let Some(last) = rows.last() else {
            break;
        };
        after = Some(last.asset_id.clone());
        scanned += rows.len();
        for row in &rows {
            for blob in row.blobs.iter().filter(|blob| blob.size == 0) {
                let reference = ZeroSized {
                    asset: row.asset_id.clone(),
                    role: blob.role,
                    address: blob.address.clone(),
                };
                let Some(stat) = context.blobs.stat(&blob.address).await? else {
                    tracing::warn!(
                        asset = %reference.asset, role = reference.role.as_str(),
                        address = %reference.address,
                        "a size-0 reference names a blob the store does not hold; kept"
                    );
                    report.missing.push(reference);
                    continue;
                };
                if stat.size == 0 {
                    tracing::warn!(
                        asset = %reference.asset, role = reference.role.as_str(),
                        address = %reference.address,
                        "a size-0 reference names a blob the store holds empty; kept"
                    );
                    report.empty.push(reference);
                    continue;
                }
                let resized = Resized {
                    reference,
                    size: stat.size,
                };
                if mode == Mode::Apply {
                    let outcome = context
                        .index
                        .repair_blob_size(
                            &resized.reference.asset,
                            resized.reference.role,
                            &resized.reference.address,
                            resized.size,
                        )
                        .await?;
                    if outcome != SizeRepair::Repaired {
                        tracing::info!(
                            asset = %resized.reference.asset,
                            role = resized.reference.role.as_str(),
                            ?outcome,
                            "a size-0 reference moved before its repair; left alone"
                        );
                        report.moved.push(resized);
                        continue;
                    }
                } else {
                    tracing::debug!(
                        asset = %resized.reference.asset,
                        role = resized.reference.role.as_str(),
                        size = resized.size,
                        "a size-0 reference would be repaired"
                    );
                }
                report.repaired.push(resized);
            }
        }
    }
    tracing::info!(
        applied = report.applied,
        scanned,
        repaired = report.repaired.len(),
        moved = report.moved.len(),
        missing = report.missing.len(),
        empty = report.empty.len(),
        "the size repair pass finished"
    );
    Ok(report)
}

#[cfg(test)]
mod tests;
