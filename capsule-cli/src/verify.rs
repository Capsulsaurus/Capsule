//! `capsule verify`: ask the server whether it durably holds this library's originals, and with
//! `--signed` keep its signed answer as evidence (SSoT:
//! [Transfer — Durability](https://docs/design/transfer/durability/)).
//!
//! The network legs are the SDK's: [`crate::remote::verify_storage`] builds a [`VerdictSource`]
//! over `capsule_sdk::verify`, which sends `POST /v1/storage/verify` and, for a signed request,
//! verifies the attestation under the server's attestation-key history before handing it back.
//! Persistence is the library's: `capsule_core::library::append_attestation` appends the verified
//! bytes to each asset's `{uuid}.attestations.cbor` log beside its custody receipts. What this
//! module owns is the orchestration between them:
//!
//! - **What is declared.** Each asset declares the blobs a release of its original waits on — the
//!   original ciphertext and, when the head binds one, the sealed metadata blob — read off the
//!   head manifest, so nothing is re-encrypted to ask.
//! - **Batching.** Assets are asked about in batches of at most [`VERIFY_BATCH`], the server's
//!   per-request limit. A signed batch is one attestation answering for every asset in it, so
//!   the same verified bytes land in the log of each.
//! - **Judgement.** An asset is durable only when the server says so *and* every declared blob
//!   re-checks as stored, indexed and retrievable; the client never trusts the aggregate alone.
//!
//! One batch's failure is its assets': each is recorded with its reason and the run moves on.
//! An attestation that did not verify is never persisted.

use std::collections::HashMap;
use std::future::Future;

use capsule_core::crypto::hash::Hash32;
use capsule_core::library::{BlobRole, StorageVerdict, release_is_safe};
use capsule_core::lifecycle::Workspace;
use capsule_sdk::verify::{AssetQuery, VerifiedAttestation, VerifyError};
use serde::Serialize;
use tracing::instrument;
use uuid::Uuid;

/// The most assets one `POST /v1/storage/verify` may ask about: the server's
/// `MAX_ASSETS_PER_REQUEST`.
pub const VERIFY_BATCH: usize = 100;

/// How a `capsule verify` run is configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyOptions {
    /// The assets to verify; empty means every asset in the library.
    pub assets: Vec<Uuid>,
    /// Ask for the signed verdict and keep it in each asset's attestation log.
    pub signed: bool,
    /// Ask the server to re-hash the stored bytes (spends the account's deep-verify budget).
    pub deep: bool,
}

/// Where the server's answers come from: the SDK in production, a stand-in in tests.
pub(crate) trait VerdictSource {
    /// The unsigned verdicts for `assets`, one per asset.
    fn verdicts(
        &self,
        assets: &[AssetQuery],
        deep: bool,
    ) -> impl Future<Output = Result<Vec<StorageVerdict>, VerifyError>>;

    /// A signed attestation for `assets`, already verified for this request under the server's
    /// attestation-key history and a fresh nonce. An attestation that does not verify is an
    /// error, never a value.
    fn attestation(
        &self,
        assets: &[AssetQuery],
        deep: bool,
    ) -> impl Future<Output = Result<VerifiedAttestation, VerifyError>>;
}

/// Where one asset ended up after a verify run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetVerifyStatus {
    /// The server holds every declared blob, stored, indexed and retrievable.
    Durable,
    /// The server answered, and does not durably hold every declared blob.
    NotDurable,
    /// No answer was obtained or kept; `error` says why.
    Failed,
}

/// Why an asset failed, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetVerifyFailure {
    /// A stable name for where the asset stopped: `unknown_asset`, `no_chain`,
    /// `verify_unavailable`, `attestation_rejected`, `missing_verdict` or `attestation_store`.
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it supplied one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
}

impl AssetVerifyFailure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: None,
            message: message.into(),
        }
    }

    fn from_verify(error: &VerifyError) -> Self {
        match error {
            VerifyError::Attestation(_) => Self::new("attestation_rejected", error.to_string()),
            VerifyError::Status { code, .. } => Self {
                kind: "verify_unavailable",
                code: code.clone(),
                message: error.to_string(),
            },
            VerifyError::Transport(_) | VerifyError::Malformed(_) => {
                Self::new("verify_unavailable", error.to_string())
            }
        }
    }
}

/// One declared blob's verdict, as `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlobVerifyOutcome {
    /// The content address, lowercase hex.
    pub hash: String,
    /// The role the server holds it under: `original`, `metadata`, `derivative` or `provenance`.
    pub role: &'static str,
    /// Present in the blob store.
    pub stored: bool,
    /// Referenced by a committed index row.
    pub indexed: bool,
    /// Nothing is withholding it.
    pub retrievable: bool,
}

/// What a verify run found for one asset — one line of the `--json` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetVerifyOutcome {
    /// The asset id.
    pub asset_id: String,
    /// Where the asset ended up.
    pub status: AssetVerifyStatus,
    /// The server's clock when it looked, when it answered.
    pub checked_at: Option<String>,
    /// One entry per declared blob, when the server answered.
    pub blobs: Vec<BlobVerifyOutcome>,
    /// Whether this run appended a signed attestation to the asset's log. `false` on an
    /// unsigned run, and on a signed run whose attestation the log already held.
    pub attestation_stored: bool,
    /// Why the asset failed, for a [`AssetVerifyStatus::Failed`] asset only.
    pub error: Option<AssetVerifyFailure>,
}

impl AssetVerifyOutcome {
    fn failed(asset_id: Uuid, error: AssetVerifyFailure) -> Self {
        Self {
            asset_id: asset_id.to_string(),
            status: AssetVerifyStatus::Failed,
            checked_at: None,
            blobs: Vec::new(),
            attestation_stored: false,
            error: Some(error),
        }
    }

    /// The outcome of a verdict answering for `declared`. Durable only under the release gate's
    /// own predicate ([`release_is_safe`]: `durable`, a non-empty blob list, every blob safely
    /// stored) **and** when every declared hash is among the blobs the verdict re-checks — a
    /// verdict that drops a declared blob has not confirmed it, whatever its aggregate says.
    fn answered(verdict: &StorageVerdict, declared: &[Hash32]) -> Self {
        let covers_declared = declared.iter().all(|hash| {
            verdict
                .blobs
                .iter()
                .any(|blob| blob.hash == *hash && blob.safely_stored())
        });
        let durable = release_is_safe(verdict, true) && covers_declared;
        Self {
            asset_id: verdict.asset_id.to_string(),
            status: if durable {
                AssetVerifyStatus::Durable
            } else {
                AssetVerifyStatus::NotDurable
            },
            checked_at: Some(verdict.checked_at.clone()),
            blobs: verdict
                .blobs
                .iter()
                .map(|blob| BlobVerifyOutcome {
                    hash: blob.hash.to_hex(),
                    role: role_name(blob.role),
                    stored: blob.stored,
                    indexed: blob.indexed,
                    retrievable: blob.retrievable,
                })
                .collect(),
            attestation_stored: false,
            error: None,
        }
    }
}

fn role_name(role: BlobRole) -> &'static str {
    match role {
        BlobRole::Original => "original",
        BlobRole::Metadata => "metadata",
        BlobRole::Derivative => "derivative",
        BlobRole::Provenance => "provenance",
    }
}

/// The outcome of a `capsule verify` run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VerifySummary {
    /// Assets considered.
    pub assets: usize,
    /// Assets the server durably holds.
    pub durable_assets: usize,
    /// Assets the server answered for and does not durably hold.
    pub not_durable_assets: usize,
    /// Assets with no answer obtained or kept; each is in `outcomes` with its error.
    pub failed_assets: usize,
    /// Whether the run asked for signed verdicts.
    pub signed: bool,
    /// Whether the run asked for a deep re-hash.
    pub deep: bool,
    /// Signed attestations appended to asset logs this run.
    pub attestations_stored: usize,
    /// One entry per asset considered, in the order asked.
    pub outcomes: Vec<AssetVerifyOutcome>,
}

impl VerifySummary {
    fn record(&mut self, outcome: AssetVerifyOutcome) {
        match outcome.status {
            AssetVerifyStatus::Durable => self.durable_assets += 1,
            AssetVerifyStatus::NotDurable => self.not_durable_assets += 1,
            AssetVerifyStatus::Failed => self.failed_assets += 1,
        }
        if outcome.attestation_stored {
            self.attestations_stored += 1;
        }
        self.outcomes.push(outcome);
    }

    /// The assets that are not durably held or got no answer.
    pub fn problems(&self) -> impl Iterator<Item = &AssetVerifyOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status != AssetVerifyStatus::Durable)
    }
}

/// The query an asset declares: its original ciphertext and, when the head binds one, its sealed
/// metadata blob — the blobs a release of its original waits on.
fn declared_query(workspace: &Workspace, asset_id: Uuid) -> Result<AssetQuery, AssetVerifyFailure> {
    let asset = workspace.asset(&asset_id).ok_or_else(|| {
        AssetVerifyFailure::new("unknown_asset", "the library holds no such asset")
    })?;
    let head = &asset
        .chain
        .records()
        .last()
        .ok_or_else(|| AssetVerifyFailure::new("no_chain", "the asset has no provenance chain"))?
        .manifest
        .core;
    let mut blob_hashes: Vec<Hash32> = vec![head.ciphertext_hash];
    blob_hashes.extend(head.metadata_blob_hash);
    Ok(AssetQuery {
        asset_id,
        blob_hashes,
    })
}

/// Verify `options.assets` (or the whole library) against `source`, keeping each signed
/// attestation in the logs of the assets it answers for.
#[instrument(skip_all, fields(signed = options.signed, deep = options.deep))]
pub(crate) async fn verify_assets<S: VerdictSource>(
    workspace: &Workspace,
    source: &S,
    options: &VerifyOptions,
) -> VerifySummary {
    verify_assets_in_batches(workspace, source, options, VERIFY_BATCH).await
}

/// [`verify_assets`] with the batch size as a parameter, so the batching is testable without a
/// library of a hundred assets.
async fn verify_assets_in_batches<S: VerdictSource>(
    workspace: &Workspace,
    source: &S,
    options: &VerifyOptions,
    batch_size: usize,
) -> VerifySummary {
    let ids = if options.assets.is_empty() {
        // Sorted, so a whole-library run reports (and batches) in a stable order.
        let mut ids = workspace.asset_ids();
        ids.sort_unstable();
        ids
    } else {
        options.assets.clone()
    };
    let mut summary = VerifySummary {
        assets: ids.len(),
        signed: options.signed,
        deep: options.deep,
        ..Default::default()
    };

    // Resolve every declaration first, so outcomes come back in the order asked.
    let mut slots: Vec<Option<AssetVerifyOutcome>> = vec![None; ids.len()];
    let mut queries: Vec<(usize, AssetQuery)> = Vec::with_capacity(ids.len());
    for (slot, &asset_id) in ids.iter().enumerate() {
        match declared_query(workspace, asset_id) {
            Ok(query) => queries.push((slot, query)),
            Err(failure) => {
                tracing::warn!(%asset_id, kind = failure.kind, "verify: nothing to declare");
                slots[slot] = Some(AssetVerifyOutcome::failed(asset_id, failure));
            }
        }
    }

    for batch in queries.chunks(batch_size.max(1)) {
        let asked: Vec<AssetQuery> = batch.iter().map(|(_, q)| q.clone()).collect();
        for (slot, outcome) in verify_batch(workspace, source, options, batch, &asked).await {
            slots[slot] = Some(outcome);
        }
    }

    for outcome in slots.into_iter().flatten() {
        summary.record(outcome);
    }
    tracing::info!(
        assets = summary.assets,
        durable = summary.durable_assets,
        not_durable = summary.not_durable_assets,
        failed = summary.failed_assets,
        attestations_stored = summary.attestations_stored,
        signed = summary.signed,
        "verify complete"
    );
    summary
}

/// Ask about one batch and turn the answer into one outcome per asset in it.
async fn verify_batch<S: VerdictSource>(
    workspace: &Workspace,
    source: &S,
    options: &VerifyOptions,
    batch: &[(usize, AssetQuery)],
    asked: &[AssetQuery],
) -> Vec<(usize, AssetVerifyOutcome)> {
    let answer = if options.signed {
        source
            .attestation(asked, options.deep)
            .await
            .map(|verified| (verified.verdicts.clone(), Some(verified)))
    } else {
        source
            .verdicts(asked, options.deep)
            .await
            .map(|verdicts| (verdicts, None))
    };
    let (verdicts, attestation) = match answer {
        Ok(answer) => answer,
        Err(error) => {
            // A refused attestation is never persisted; every asset it would have covered
            // fails with the reason.
            tracing::warn!(%error, assets = batch.len(), "verify: the batch got no answer");
            let failure = AssetVerifyFailure::from_verify(&error);
            return batch
                .iter()
                .map(|(slot, q)| {
                    (
                        *slot,
                        AssetVerifyOutcome::failed(q.asset_id, failure.clone()),
                    )
                })
                .collect();
        }
    };
    let by_asset: HashMap<Uuid, &StorageVerdict> =
        verdicts.iter().map(|v| (v.asset_id, v)).collect();

    batch
        .iter()
        .map(|(slot, query)| {
            let Some(verdict) = by_asset.get(&query.asset_id) else {
                return (
                    *slot,
                    AssetVerifyOutcome::failed(
                        query.asset_id,
                        AssetVerifyFailure::new(
                            "missing_verdict",
                            "the server's answer omits this asset",
                        ),
                    ),
                );
            };
            let mut outcome = AssetVerifyOutcome::answered(verdict, &query.blob_hashes);
            if let Some(verified) = &attestation {
                match keep_attestation(workspace, query.asset_id, &verified.cbor) {
                    Ok(appended) => outcome.attestation_stored = appended,
                    Err(failure) => {
                        // The verdict is known, but the evidence of it is not kept: that is the
                        // one thing a signed run promises, so the asset fails.
                        outcome.status = AssetVerifyStatus::Failed;
                        outcome.error = Some(failure);
                    }
                }
            }
            (*slot, outcome)
        })
        .collect()
}

/// Append a verified attestation to an asset's log, beside its receipts.
fn keep_attestation(
    workspace: &Workspace,
    asset_id: Uuid,
    cbor: &[u8],
) -> Result<bool, AssetVerifyFailure> {
    let capture_utc = workspace.asset(&asset_id).map(|asset| asset.capture_utc);
    capsule_core::library::append_attestation(workspace.root(), &asset_id, capture_utc, cbor)
        .map_err(|error| {
            tracing::warn!(%asset_id, %error, "verify: the attestation could not be kept");
            AssetVerifyFailure::new(
                "attestation_store",
                format!("the attestation could not be stored in the library: {error}"),
            )
        })
}

/// The line after a run: how many assets the server durably holds, and what was kept. English
/// inline, as every CLI line is going to be (issue #680).
#[must_use]
pub fn summary_line(summary: &VerifySummary) -> String {
    let line = format!(
        "{} of {} assets durably held by the server; {} not durable, {} failed.",
        summary.durable_assets, summary.assets, summary.not_durable_assets, summary.failed_assets
    );
    if summary.signed {
        format!(
            "{line} {} signed attestations stored in the library.",
            summary.attestations_stored
        )
    } else {
        line
    }
}

/// One asset that is not durably held or got no answer, as the line the human output prints.
#[must_use]
pub fn problem_line(outcome: &AssetVerifyOutcome) -> String {
    if let Some(error) = &outcome.error {
        return format!("asset {} failed: {}", outcome.asset_id, error.message);
    }
    let missing: Vec<&str> = outcome
        .blobs
        .iter()
        .filter(|b| !(b.stored && b.indexed && b.retrievable))
        .map(|b| b.role)
        .collect();
    format!(
        "asset {} is not durably held (not safely stored: {})",
        outcome.asset_id,
        if missing.is_empty() {
            "the server's answer does not confirm every declared blob".to_owned()
        } else {
            missing.join(", ")
        }
    )
}

/// Why a finished run exits non-zero, or `None` when every asset is durably held.
#[must_use]
pub fn failed_run_reason(summary: &VerifySummary) -> Option<String> {
    let problems = summary.not_durable_assets + summary.failed_assets;
    (problems > 0).then(|| {
        format!(
            "{problems} of {} assets are not confirmed durable on the server",
            summary.assets
        )
    })
}

#[cfg(test)]
mod tests;
