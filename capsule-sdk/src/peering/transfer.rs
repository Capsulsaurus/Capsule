//! What moves over the wire, and how it is applied.
//!
//! The transfer payload is a **delta-scoped [backup artifact]** — backup artifacts are
//! explicitly *"constructed from a list of assets, albums, and so on,"* so a delta-scoped one
//! needs no special construction path ([`build_delta_artifact`]). It is fetched with ranged
//! `GET` ([`pull_artifact`] over the shared [`RangedFetcher`]), which makes the transfer
//! **resumable** across the flaky-by-nature LAN and **idempotent** — content-addressing turns a
//! re-fetch of an already-held blob into a no-op.
//!
//! A received artifact is ingested through the **backup restore path** ([`ingest`]) — peering
//! adds no separate deserialization. Restore re-verifies every ciphertext hash, checks STREAM
//! tags on decrypt, and runs each manifest through `verify_asset`. Peering adds one thing on top:
//! **chain-aware forward-vs-stale reconciliation**. A peer pull cannot resurrect an asset the
//! local device has already superseded — even if the artifact was honestly produced from an
//! older state of the sender. Such an asset is **quarantined**, never silently applied.
//!
//! [backup artifact]: https://docs/design/backup-recovery/#backup-artifact

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use capsule_core::backup::artifact::RestoredAsset;
use capsule_core::backup::{BackupArtifact, BackupAsset, BackupInput, RestoreMode, export};
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{HybridVerifyingKey, Signer};
use tracing::instrument;
use uuid::Uuid;

use super::PeeringError;
use crate::fetch::{BlobSource, RangeOutcome, RangedFetcher};
use crate::net::ConnectionClass;

/// The ciphertext content address of a backup asset — the head manifest's `ciphertext_hash`,
/// which is what a delta is expressed over.
fn asset_address(asset: &BackupAsset) -> Option<Hash32> {
    asset
        .provenance
        .last()
        .map(|record| record.manifest.core.ciphertext_hash)
}

/// Everything needed to assemble a delta-scoped artifact on the sending device.
pub struct DeltaExport<'a> {
    /// The sender's full candidate asset set (only those in `delta` are included).
    pub assets: &'a [BackupAsset],
    /// The content addresses the receiver is missing — the delta to include.
    pub delta: &'a BTreeSet<Hash32>,
    /// The AMKs for every `(album, epoch)` an included asset references.
    pub amks: &'a BTreeMap<(Uuid, u32), [u8; 32]>,
    /// The exporting device id (provenance: who produced the artifact).
    pub exporter_device: Uuid,
    /// The sender's library version string.
    pub source_library_version: String,
    /// RFC 3339 export timestamp.
    pub export_timestamp: String,
}

/// Build a delta-scoped backup artifact: include exactly the sender's assets whose content
/// address is in `delta`, then export through the ordinary backup path. Assets the receiver
/// already holds are never in `delta`, so they never enter the artifact — the "only missing
/// assets transfer" guarantee is structural, decided before a single byte is sealed.
#[instrument(skip(export_input, passphrase, signer), fields(delta = export_input.delta.len()))]
pub fn build_delta_artifact(
    export_input: &DeltaExport<'_>,
    passphrase: &[u8],
    signer: &dyn Signer,
) -> Result<Vec<u8>, PeeringError> {
    let assets: Vec<BackupAsset> = export_input
        .assets
        .iter()
        .filter(|asset| asset_address(asset).is_some_and(|addr| export_input.delta.contains(&addr)))
        .cloned()
        .collect();
    tracing::debug!(
        included = assets.len(),
        candidates = export_input.assets.len(),
        "assembled delta-scoped backup artifact"
    );
    let input = BackupInput {
        assets,
        amks: export_input.amks.clone(),
        exporter_device: export_input.exporter_device,
        source_library_version: export_input.source_library_version.clone(),
        export_timestamp: export_input.export_timestamp.clone(),
    };
    Ok(export(&input, passphrase, signer)?)
}

/// The content address (SHA-256 hex) of an assembled artifact, for the ranged-GET integrity
/// check. Byte-identical to the media server's `/blob/{hash}` addressing.
#[must_use]
pub fn artifact_address(bytes: &[u8]) -> String {
    hash_bytes(bytes).to_hex()
}

/// Pull an artifact over ranged `GET`, reusing the adverse-hardened [`RangedFetcher`]. Each
/// request resumes from the current buffer length, so an interruption re-fetches **zero** bytes
/// already held, and the reassembled bytes must hash to `artifact_hash` or the transfer fails —
/// the same resumability and content-address verification the server download path relies on.
#[instrument(skip(source), fields(len))]
pub async fn pull_artifact<B: BlobSource>(
    source: &B,
    artifact_hash: &str,
    len: u64,
) -> Result<Vec<u8>, PeeringError> {
    RangedFetcher::new(ConnectionClass::Unmetered)
        .fetch(source, artifact_hash, len)
        .await
        .map_err(|e| PeeringError::Transfer(e.to_string()))
}

/// The outcome of ingesting a peered artifact: assets to apply, assets quarantined as stale, and
/// assets already current locally.
#[derive(Debug, Default)]
pub struct PeerRestore {
    /// Decrypted, verified assets to write — new assets and **forward** updates the local head is
    /// an ancestor of.
    pub applied: Vec<RestoredAsset>,
    /// Assets whose artifact head the local device has already superseded (or diverged from):
    /// **quarantined**, never applied — "peer sent stale state".
    pub quarantined: Vec<Uuid>,
    /// Assets already at the artifact's head locally — a no-op.
    pub identical: Vec<Uuid>,
}

/// Ingest a peered artifact through the backup restore path with chain-aware reconciliation.
///
/// For each asset the artifact carries, relative to `local_heads`:
/// - **absent locally** → applied (a new asset);
/// - **head identical** → no-op;
/// - **head differs, artifact chain contains our head as an ancestor** → a *forward* update, so
///   we adopt it (applied);
/// - **head differs otherwise** → the artifact is stale or divergent; **quarantined**, never
///   applied, so a peer can never resurrect a locally-superseded asset.
///
/// Applying is delegated to [`BackupArtifact::restore`] in `Commit` mode (which decrypts and
/// re-verifies), driven by a reconciled head map so forward assets restore and stale ones fall
/// into restore's conflict bucket.
#[instrument(skip(artifact_bytes, passphrase, exporter_pub, local_heads), fields(bytes = artifact_bytes.len()))]
pub fn ingest(
    artifact_bytes: &[u8],
    passphrase: &[u8],
    exporter_pub: &HybridVerifyingKey,
    local_heads: &BTreeMap<Uuid, Hash32>,
) -> Result<PeerRestore, PeeringError> {
    let artifact = BackupArtifact::open(artifact_bytes, passphrase, exporter_pub)?;

    // The head map restore reconciles against — forward assets are removed so restore adds them,
    // stale assets keep their local head so restore quarantines them as conflicts.
    let mut restore_heads = local_heads.clone();
    let mut quarantined = Vec::new();
    let mut identical = Vec::new();

    for (asset_id, artifact_head) in artifact.provenance_heads() {
        match local_heads.get(asset_id) {
            None => {} // new asset: absent in restore_heads → would_add → applied
            Some(local) if local == artifact_head => identical.push(*asset_id),
            Some(local) => {
                let chain = artifact.provenance_chain(asset_id)?.unwrap_or_default();
                let is_forward = chain.iter().any(|record| record.record_hash() == *local);
                if is_forward {
                    // Adopt the newer state: drop our older head so restore applies it.
                    restore_heads.remove(asset_id);
                } else {
                    tracing::warn!(asset = %asset_id, "peer sent stale state; quarantining");
                    quarantined.push(*asset_id);
                }
            }
        }
    }

    let report = artifact.restore(RestoreMode::Commit, &restore_heads)?;
    Ok(PeerRestore {
        applied: report.applied,
        quarantined,
        identical,
    })
}

// ── In-memory ranged source ───────────────────────────────────────────────────

/// An in-memory [`BlobSource`] that serves an artifact's bytes over `Range` requests — the
/// deterministic stand-in for the peering channel's ranged `GET`. It records every served range
/// (so a test can prove **zero duplicate bytes** on resume) and can inject a single mid-transfer
/// **LAN drop** (a short read) to exercise the resume path.
#[derive(Clone)]
pub struct ArtifactBlobSource {
    bytes: Arc<Vec<u8>>,
    served: Arc<Mutex<Vec<(u64, u64)>>>,
    drop_after: Arc<Mutex<Option<u64>>>,
}

impl ArtifactBlobSource {
    /// Serve `bytes` with no injected drop.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::new(bytes),
            served: Arc::new(Mutex::new(Vec::new())),
            drop_after: Arc::new(Mutex::new(None)),
        }
    }

    /// Serve `bytes`, but truncate the **first** ranged response to `drop_after` bytes to
    /// simulate a LAN drop mid-transfer. The next request resumes from the persisted offset.
    #[must_use]
    pub fn with_drop_after(bytes: Vec<u8>, drop_after: u64) -> Self {
        Self {
            bytes: Arc::new(bytes),
            served: Arc::new(Mutex::new(Vec::new())),
            drop_after: Arc::new(Mutex::new(Some(drop_after))),
        }
    }

    /// The total artifact length.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Whether the artifact is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The `(start, len)` of every range served so far, in order.
    #[must_use]
    pub fn served_ranges(&self) -> Vec<(u64, u64)> {
        self.served.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl BlobSource for ArtifactBlobSource {
    async fn get_range(&self, _hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        let total = self.bytes.len() as u64;
        if start >= total {
            return RangeOutcome::Complete { bytes: Vec::new() };
        }
        let remaining = total - start;
        let mut give = max_len.map_or(remaining, |m| m.min(remaining));

        // Inject a one-time short read (a LAN drop) if configured.
        let mut partial = false;
        if let Ok(mut guard) = self.drop_after.lock()
            && let Some(n) = guard.take()
            && n < give
        {
            give = n;
            partial = true;
        }

        let end = start + give;
        let slice = self.bytes[start as usize..end as usize].to_vec();
        if let Ok(mut guard) = self.served.lock() {
            guard.push((start, give));
        }

        if partial {
            RangeOutcome::Partial { bytes: slice }
        } else {
            RangeOutcome::Complete { bytes: slice }
        }
    }
}
