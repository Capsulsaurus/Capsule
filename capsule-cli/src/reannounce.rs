//! `capsule sync reannounce`: give the server back the signed manifests this library holds and
//! the server's feed no longer lists (#781).
//!
//! After an index rebuild moved the library to a new sync epoch, `capsule sync` meets
//! `409 error.sync.cursor_stale`, resyncs from zero, and runs this; it also runs on demand. The
//! network half is [`crate::remote::reannounce`], over the SDK's
//! [`reannounce_unlisted`](capsule_sdk::sync::reannounce::reannounce_unlisted); this module is
//! the pure half: which manifests the library holds, and the `--json` report of what the server
//! answered.
//!
//! A deleted asset is not announced: the server does not restore tombstones, so sending one
//! would only cost a request and a refusal.

use capsule_core::crypto::provenance::Action;
use capsule_core::lifecycle::{LifecycleError, Workspace};
use capsule_sdk::sync::reannounce::{HeldManifest, ReannounceOutcome, ReannounceResult};
use serde::Serialize;

/// Every live asset's chain-head provenance record, and how many deleted assets were left out.
///
/// The record is encoded exactly as an upload's `provenance` blob is
/// ([`Workspace::upload_bundle`]), without building the rest of the bundle: re-announce sends
/// manifests only, so it never reads an original.
///
/// # Errors
///
/// [`LifecycleError::Cbor`] when a record does not encode, which is a corrupt library.
pub fn held_manifests(workspace: &Workspace) -> Result<(Vec<HeldManifest>, usize), LifecycleError> {
    let mut held = Vec::new();
    let mut deleted = 0;
    for asset_id in workspace.asset_ids() {
        let Some(asset) = workspace.asset(&asset_id) else {
            continue;
        };
        let Some(head) = asset.chain.records().last() else {
            continue;
        };
        if head.manifest.core.action == Action::Delete {
            deleted += 1;
            continue;
        }
        let manifest = capsule_core::cbor::to_canonical_vec(head)
            .map_err(|e| LifecycleError::Cbor(format!("encoding the provenance record: {e}")))?;
        held.push(HeldManifest {
            asset_id: asset_id.to_string(),
            manifest,
        });
    }
    Ok((held, deleted))
}

/// Where one asset ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetReannounceStatus {
    /// Dry run: it would have been re-announced, and nothing was sent.
    Planned,
    /// The server restored its row.
    Restored,
    /// The server already held its row.
    AlreadyIndexed,
    /// The server lacks blobs it needs; `capsule push` uploads them again.
    MissingBlobs,
    /// The server refused it; `code` says why.
    Refused,
}

/// One asset's line of the `--json` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetReannounceOutcome {
    /// The asset id.
    pub asset_id: String,
    /// Where it ended up.
    pub status: AssetReannounceStatus,
    /// The position a restored row was published at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_seq: Option<u64>,
    /// The content addresses the server lacks, for `missing_blobs`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The server's stable `error.sync.reannounce_*` code, for `refused`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// What a `capsule sync reannounce` run did — the `--json` document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReannounceSummary {
    /// Nothing was sent.
    pub dry_run: bool,
    /// Live assets the library holds.
    pub held: usize,
    /// Deleted assets left out.
    pub skipped_deleted: usize,
    /// Assets the server's feed lists.
    pub listed: usize,
    /// Assets re-announced (planned, on a dry run).
    pub reannounced: usize,
    /// Rows the server restored.
    pub restored: usize,
    /// Rows the server already held.
    pub already_indexed: usize,
    /// Assets whose blobs the server lacks: `capsule push` re-uploads them.
    pub missing_blobs: usize,
    /// Assets the server refused.
    pub refused: usize,
    /// One line per re-announced asset, in the order sent.
    pub assets: Vec<AssetReannounceOutcome>,
}

impl ReannounceSummary {
    /// The report of a dry run: what would be sent.
    pub fn planned(
        held: usize,
        skipped_deleted: usize,
        listed: usize,
        unlisted: &[&HeldManifest],
    ) -> Self {
        Self {
            dry_run: true,
            held,
            skipped_deleted,
            listed,
            reannounced: unlisted.len(),
            assets: unlisted
                .iter()
                .map(|manifest| AssetReannounceOutcome {
                    asset_id: manifest.asset_id.clone(),
                    status: AssetReannounceStatus::Planned,
                    sync_seq: None,
                    missing: Vec::new(),
                    code: None,
                })
                .collect(),
            ..Self::default()
        }
    }

    /// The report of a run: what the server answered for each asset sent.
    pub fn answered(
        held: usize,
        skipped_deleted: usize,
        listed: usize,
        answers: Vec<(String, ReannounceResult)>,
    ) -> Self {
        let mut summary = Self {
            held,
            skipped_deleted,
            listed,
            reannounced: answers.len(),
            ..Self::default()
        };
        for (asset_id, result) in answers {
            let mut line = AssetReannounceOutcome {
                asset_id,
                status: AssetReannounceStatus::Restored,
                sync_seq: None,
                missing: Vec::new(),
                code: None,
            };
            match result.outcome {
                ReannounceOutcome::Restored { sync_seq } => {
                    summary.restored += 1;
                    line.sync_seq = sync_seq;
                }
                ReannounceOutcome::AlreadyIndexed => {
                    summary.already_indexed += 1;
                    line.status = AssetReannounceStatus::AlreadyIndexed;
                }
                ReannounceOutcome::MissingBlobs(missing) => {
                    summary.missing_blobs += 1;
                    line.status = AssetReannounceStatus::MissingBlobs;
                    line.missing = missing;
                }
                ReannounceOutcome::Refused { code } => {
                    summary.refused += 1;
                    line.status = AssetReannounceStatus::Refused;
                    line.code = Some(code);
                }
            }
            summary.assets.push(line);
        }
        summary
    }

    /// Log the run's totals.
    pub fn log(&self) {
        tracing::info!(
            dry_run = self.dry_run,
            held = self.held,
            skipped_deleted = self.skipped_deleted,
            listed = self.listed,
            reannounced = self.reannounced,
            restored = self.restored,
            already_indexed = self.already_indexed,
            missing_blobs = self.missing_blobs,
            refused = self.refused,
            "re-announce finished"
        );
    }
}

#[cfg(test)]
mod tests;
