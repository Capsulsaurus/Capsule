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
//! tags on decrypt, and runs each manifest through `verify_asset`. Peering adds on top:
//!
//! - **Chain-aware forward-vs-stale reconciliation.** A peer pull cannot resurrect an asset the
//!   local device has already superseded — even if the artifact was honestly produced from an
//!   older state of the sender. Such an asset is **quarantined**, never silently applied.
//! - **No resurrection.** An asset the device holds a tombstone for (kept past purge) is refused.
//! - **All-or-nothing per asset.** Each asset restores on its own: one that fails to decrypt, or
//!   arrives without the metadata its manifest commits to, is not applied, and does not stop the
//!   rest.
//!
//! The artifact is sealed under the session's [`ArtifactSecret`], derived from the channel's TLS
//! exporter — never a user passphrase.
//!
//! Over a verified peering channel the ranged pull runs through [`ChannelBlobSource`]
//! ([`pull_over_channel`]): the same fetch engine, the same content-address check, and a pull
//! severed with the channel resumes on the next channel after the bytes it already holds.
//!
//! [backup artifact]: https://docs/design/backup-recovery/#backup-artifact

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use capsule_core::backup::{
    BackupArtifact, BackupAsset, BackupInput, RestoreMode, RestoredAsset, export,
};
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{HybridVerifyingKey, Signer};
use tracing::instrument;
use uuid::Uuid;

use super::PeeringError;
use super::channel::{ArtifactSecret, MAX_ARTIFACT_LEN, PeerClient, RangeFailure};
use crate::fetch::{
    BlobSource, FetchError, FetchScope, RangeOutcome, RangedFetcher, Representation,
};
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
    /// The receiver's synchronization scope. A backup artifact always carries each asset's
    /// original, so only a receiver that eagerly holds originals can take one.
    pub scope: FetchScope,
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
///
/// The receiver's scope is honoured: a receiver whose scope does not eagerly hold originals is
/// refused with [`PeeringError::OutOfScope`] rather than sent bytes it would not keep; it syncs
/// from the server, where its scope is served representation by representation.
#[instrument(skip(export_input, secret, signer), fields(delta = export_input.delta.len()))]
pub fn build_delta_artifact(
    export_input: &DeltaExport<'_>,
    secret: &ArtifactSecret,
    signer: &dyn Signer,
) -> Result<Vec<u8>, PeeringError> {
    if !export_input
        .scope
        .eager_representations()
        .contains(&Representation::Original)
    {
        tracing::debug!(scope = ?export_input.scope, "receiver scope excludes originals");
        return Err(PeeringError::OutOfScope(export_input.scope));
    }
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
        // A peer pull moves assets between devices that already hold the album and its
        // authority; `ingest` folds in no keys, so the delta carries no authority ledger.
        authorities: BTreeMap::new(),
        // Label records travel the sealed-document path between members, not the asset delta.
        docs: Vec::new(),
        // The pinned attestation-key history is the library's, not an asset's: it rides a
        // backup of the whole library, never a peer's asset delta.
        attestation_key_pins: Vec::new(),
        exporter_device: export_input.exporter_device,
        source_library_version: export_input.source_library_version.clone(),
        export_timestamp: export_input.export_timestamp.clone(),
    };
    Ok(export(&input, secret.as_bytes(), signer)?)
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

/// The receiving device's state an ingest reconciles against.
#[derive(Debug, Clone, Copy)]
pub struct LocalLibrary<'a> {
    /// Each held asset's current provenance head (`asset_id -> head record hash`).
    pub heads: &'a BTreeMap<Uuid, Hash32>,
    /// Every asset this device has tombstoned, kept past purge: never resurrected by a peer.
    pub tombstones: &'a BTreeSet<Uuid>,
}

/// The outcome of ingesting a peered artifact: every asset the artifact carried lands in exactly
/// one bucket.
#[derive(Debug, Default)]
pub struct PeerRestore {
    /// Decrypted, verified, complete assets to write — new assets and **forward** updates the
    /// local head is an ancestor of.
    pub applied: Vec<RestoredAsset>,
    /// Assets whose artifact head the local device has already superseded (or diverged from):
    /// **quarantined**, never applied — "peer sent stale state".
    pub quarantined: Vec<Uuid>,
    /// Assets already at the artifact's head locally — a no-op.
    pub identical: Vec<Uuid>,
    /// Assets this device holds a tombstone for: refused, never resurrected.
    pub tombstoned: Vec<Uuid>,
    /// Assets that failed to restore or arrived without the metadata their manifest commits
    /// to: not applied, in whole.
    pub incomplete: Vec<Uuid>,
}

/// Ingest a peered artifact through the backup restore path with chain-aware reconciliation.
///
/// For each asset the artifact carries, relative to `local`:
/// - **tombstoned locally** → refused, whatever its chain says;
/// - **absent locally** → applied (a new asset);
/// - **head identical** → no-op;
/// - **head differs, artifact chain contains our head as an ancestor** → a *forward* update, so
///   we adopt it (applied);
/// - **head differs otherwise** → the artifact is stale or divergent; **quarantined**, never
///   applied, so a peer can never resurrect a locally-superseded asset.
///
/// Each asset to apply restores on its own through [`BackupArtifact::restore`] in `Commit` mode
/// (which decrypts and re-verifies), so it applies whole or not at all: a restore failure, or a
/// metadata blob that is missing or does not match the hash its manifest commits to, lands it in
/// [`PeerRestore::incomplete`] without touching the others.
#[instrument(skip_all, fields(bytes = artifact_bytes.len()))]
pub fn ingest(
    artifact_bytes: &[u8],
    secret: &ArtifactSecret,
    exporter_pub: &HybridVerifyingKey,
    local: LocalLibrary<'_>,
) -> Result<PeerRestore, PeeringError> {
    let artifact = BackupArtifact::open(artifact_bytes, secret.as_bytes(), exporter_pub)?;
    let mut outcome = PeerRestore::default();
    let mut to_apply = Vec::new();

    for (asset_id, artifact_head) in artifact.provenance_heads() {
        if local.tombstones.contains(asset_id) {
            tracing::warn!(asset = %asset_id, "peer offered a tombstoned asset; refusing");
            outcome.tombstoned.push(*asset_id);
            continue;
        }
        match local.heads.get(asset_id) {
            None => to_apply.push(*asset_id),
            Some(head) if head == artifact_head => outcome.identical.push(*asset_id),
            Some(head) => {
                let chain = artifact.provenance_chain(asset_id)?.unwrap_or_default();
                if chain.iter().any(|record| record.record_hash() == *head) {
                    to_apply.push(*asset_id);
                } else {
                    tracing::warn!(asset = %asset_id, "peer sent stale state; quarantining");
                    outcome.quarantined.push(*asset_id);
                }
            }
        }
    }

    // A head map under which every carried asset reads as identical, so a restore against it
    // with one asset's entry removed applies exactly that asset.
    let mut isolate: BTreeMap<Uuid, Hash32> = artifact.provenance_heads().iter().copied().collect();
    for asset_id in to_apply {
        let held = isolate.remove(&asset_id);
        let restored = artifact
            .restore(RestoreMode::Commit, &isolate)
            .map(|report| report.applied);
        if let Some(head) = held {
            isolate.insert(asset_id, head);
        }
        match restored {
            Ok(mut applied) if applied.len() == 1 && carries_committed_metadata(&applied[0]) => {
                outcome.applied.append(&mut applied);
            }
            Ok(_) => {
                tracing::warn!(asset = %asset_id, "peer sent a partial asset; not applying it");
                outcome.incomplete.push(asset_id);
            }
            Err(error) => {
                tracing::warn!(asset = %asset_id, %error, "peer asset failed to restore");
                outcome.incomplete.push(asset_id);
            }
        }
    }
    tracing::debug!(
        applied = outcome.applied.len(),
        quarantined = outcome.quarantined.len(),
        identical = outcome.identical.len(),
        tombstoned = outcome.tombstoned.len(),
        incomplete = outcome.incomplete.len(),
        "peer artifact ingested"
    );
    Ok(outcome)
}

/// Whether a restored asset carries the metadata its chain requires: the exact blob the newest
/// metadata-bearing manifest commits to, or — for a chain that commits to none — a non-empty one.
fn carries_committed_metadata(asset: &RestoredAsset) -> bool {
    let committed = asset
        .provenance
        .iter()
        .rev()
        .find_map(|record| record.manifest.core.metadata_blob_hash);
    match committed {
        Some(hash) => hash_bytes(&asset.metadata_blob) == hash,
        None => !asset.metadata_blob.is_empty(),
    }
}

// ── Over the peering channel ─────────────────────────────────────────────────

/// A [`BlobSource`] over a verified peering channel ([`PeerClient`]), resuming after `held`: the
/// bytes of the artifact this device already holds from an earlier, severed pull. A range inside
/// `held` is answered locally; only the bytes after it cross the channel, so a resumed pull
/// re-fetches nothing it already holds. Every byte that arrives is appended to `held`, so a pull
/// severed again keeps what it got.
pub struct ChannelBlobSource<'a> {
    client: &'a PeerClient,
    held: Mutex<Vec<u8>>,
    failure: Mutex<Option<PeeringError>>,
}

impl<'a> ChannelBlobSource<'a> {
    /// A source over `client` that already holds `held`, the artifact's first bytes.
    #[must_use]
    pub fn resuming(client: &'a PeerClient, held: Vec<u8>) -> Self {
        Self {
            client,
            held: Mutex::new(held),
            failure: Mutex::new(None),
        }
    }

    /// The bytes held so far, and the channel failure that stopped the pull, if one did.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Option<PeeringError>) {
        (
            self.held
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner),
            self.failure
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl BlobSource for ChannelBlobSource<'_> {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        {
            let held = self.held();
            let held_len = held.len() as u64;
            if start < held_len {
                let end = max_len.map_or(held_len, |max| held_len.min(start.saturating_add(max)));
                return RangeOutcome::Partial {
                    bytes: held[start as usize..end as usize].to_vec(),
                };
            }
            if start > held_len {
                // The fetch resumes from what it received, which is what is held; a gap would
                // be a bug, and is refused rather than papered over.
                return RangeOutcome::Status {
                    status: 0,
                    code: None,
                };
            }
        }
        match self.client.range(hash, start, max_len).await {
            Ok(bytes) => {
                self.held().extend_from_slice(&bytes);
                RangeOutcome::Complete { bytes }
            }
            Err(RangeFailure { received, error }) => {
                tracing::debug!(%error, received = received.len(), "peering: a ranged pull broke off");
                self.held().extend_from_slice(&received);
                *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                if received.is_empty() {
                    RangeOutcome::Status {
                        status: 0,
                        code: None,
                    }
                } else {
                    RangeOutcome::Partial { bytes: received }
                }
            }
        }
    }
}

/// Pull the artifact `address` (`len` bytes) from the serving peer over `client`, through the
/// same resumable, content-address-verified ranged fetch as [`pull_artifact`].
///
/// `held` is what an earlier, severed pull of the same artifact received (empty for a fresh
/// pull): the pull resumes after it and **re-fetches none of it**. On success the whole verified
/// artifact is returned and `held` is left empty; when the channel fails, `held` keeps every
/// byte received so far for the next attempt; when the bytes do not hash to `address`, `held`
/// is cleared, since some of it is wrong.
///
/// # Errors
///
/// The channel's failure ([`PeeringError::PeerRefused`], [`PeeringError::Io`],
/// [`PeeringError::Timeout`], …), [`PeeringError::Codec`] for a length past
/// [`MAX_ARTIFACT_LEN`], or [`PeeringError::Transfer`] when the bytes do not verify.
#[instrument(skip(client, held), fields(peer = %client.peer().device_id, held = held.len()))]
pub async fn pull_over_channel(
    client: &PeerClient,
    address: &str,
    len: u64,
    held: &mut Vec<u8>,
) -> Result<Vec<u8>, PeeringError> {
    if len > MAX_ARTIFACT_LEN {
        return Err(PeeringError::Codec(
            "the artifact exceeds the pull ceiling".into(),
        ));
    }
    if held.len() as u64 > len {
        held.clear();
    }
    let source = ChannelBlobSource::resuming(client, std::mem::take(held));
    // The fetch hashes every byte as it passes; the source keeps them, so the sink keeps none.
    let fetched = RangedFetcher::new(ConnectionClass::Unmetered)
        .fetch_into(&source, address, len, &mut std::io::sink())
        .await;
    let (bytes, failure) = source.into_parts();
    match fetched {
        Ok(_) => {
            tracing::info!(address, len, "peering: artifact pulled and verified");
            Ok(bytes)
        }
        Err(FetchError::IntegrityFailed { .. }) => {
            tracing::warn!(address, "peering: pulled bytes do not hash to the address");
            Err(PeeringError::Transfer(format!(
                "the pulled artifact does not hash to {address}"
            )))
        }
        Err(error) => {
            *held = bytes;
            Err(failure.unwrap_or_else(|| PeeringError::Transfer(error.to_string())))
        }
    }
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
