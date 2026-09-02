//! Pushing a locally-imported asset to a server (slice `S-D18`) — the glue between
//! `capsule_core`'s [`UploadBundle`] and the hand-written [upload client](crate::upload).
//!
//! `capsule-core` builds the bundle (ciphertext, sealed metadata blob, derivatives, and the
//! manifest-envelope projection); this module turns it into the protocol's `POST /upload`
//! bodies and drives them in [tier ladder](crate::staged) order. Nothing here re-implements a
//! network flow — every byte goes through [`UploadClient`], and the ordering/gating goes
//! through [`StagedScheduler::plan_sessions`].
//!
//! **Resume derives from server truth.** There is no push-side state file: pull the sync feed,
//! fold it with [`held_from_feed`](crate::staged::held_from_feed) into the set of blob content
//! addresses the server durably holds, and [`remaining_tiers`](crate::staged::remaining_tiers)
//! rebuilds exactly the outstanding work. Within a blob, `POST /upload`'s idempotent re-create
//! returns the authoritative offset; across an asset's blobs, a `duplicate_blob` answer is a
//! merge, not an error. Re-running a push against an unchanged library is therefore a no-op.
//!
//! **Every blob this module ships is ciphertext.** The original is re-derived from its
//! manifest's nonce prefix, the metadata blob is carried sealed, and **derivative blobs are
//! encrypted too** — `capsule-core` re-derives each one from the plaintext it holds locally
//! using the prefix that derivative's signed manifest recorded. Nothing here decrypts, encrypts,
//! or inspects a blob; it moves opaque bytes.
//!
//! **One deviation from "the envelope mirrors the signed manifest", and it is the server's
//! rule:** invariant 15 requires `manifest_envelope.ciphertext_hash == hash` (the top-level
//! declared content address of *this* blob). A bundle's metadata and derivative blobs are not
//! the original, so their envelope carries their own content address; every other field is the
//! head manifest's, verbatim. See [`envelope_for`].

use std::collections::HashSet;

use capsule_core::import::UploadTier;
use capsule_core::lifecycle::UploadBundle;
use serde::Serialize;
use tracing::instrument;

use crate::albums::{AlbumClient, AlbumError, ProvisionedAlbum};
use crate::staged::{StagedAsset, StagedScheduler, TierBlob, TierSessionOutcome, remaining_tiers};
use crate::upload::{
    BlobRole, CreateUploadRequest, ManifestEnvelope, UploadClient, UploadError, UploadOutcome,
};

/// The content type a blob that is opaque ciphertext declares. The sealed metadata blob is
/// AMK ciphertext, not an image — the server's closed content-type enum (invariant 5) admits
/// exactly this for it.
const OPAQUE_CONTENT_TYPE: &str = "application/octet-stream";

// ─── Blob view over a bundle ──────────────────────────────────────────────────

/// One transferable blob of an [`UploadBundle`]: its ladder tier, protocol blob role, declared
/// content type, and the bytes themselves (borrowed from the bundle). Paired with its content
/// address by [`bundle_blobs`].
#[derive(Debug, Clone, Copy)]
pub struct BundleBlob<'a> {
    /// Which rung of the upload ladder this blob is.
    pub tier: UploadTier,
    /// The blob's role within the asset bundle.
    pub role: BlobRole,
    /// The MIME type declared for this blob.
    pub content_type: &'a str,
    /// The bytes to transfer.
    pub bytes: &'a [u8],
}

/// Every transferable blob of `bundle`, in ladder order: the sealed metadata blob (T0, the
/// index tier that makes the asset visible), each derivative (T1), then the original (T2).
///
/// A bundle whose head action binds no metadata blob (`delete`, `trash-restore`, …) simply has
/// no T0 blob — the ladder is whatever the manifest actually commits to, never a fabrication.
#[must_use]
pub fn bundle_blobs(bundle: &UploadBundle) -> Vec<(BundleBlob<'_>, String)> {
    let mut blobs = Vec::with_capacity(2 + bundle.derivatives.len());
    if let Some(hash) = &bundle.metadata_blob_hash {
        blobs.push((
            BundleBlob {
                tier: UploadTier::Index,
                role: BlobRole::Metadata,
                content_type: OPAQUE_CONTENT_TYPE,
                bytes: &bundle.metadata_blob,
            },
            hash.to_hex(),
        ));
    }
    for derivative in &bundle.derivatives {
        blobs.push((
            BundleBlob {
                tier: UploadTier::Preview,
                role: BlobRole::Derivative,
                content_type: &derivative.format,
                bytes: &derivative.bytes,
            },
            derivative.ciphertext_hash.to_hex(),
        ));
    }
    blobs.push((
        BundleBlob {
            tier: UploadTier::Original,
            role: BlobRole::Original,
            content_type: &bundle.content_type,
            bytes: &bundle.ciphertext,
        },
        bundle.ciphertext_hash.to_hex(),
    ));
    blobs
}

/// The bundle as a [`StagedAsset`] the scheduler can order and gate.
#[must_use]
pub fn staged_asset(bundle: &UploadBundle) -> StagedAsset {
    StagedAsset::new(
        bundle.asset_id.to_string(),
        bundle_blobs(bundle)
            .into_iter()
            .map(|(blob, hash)| TierBlob::new(blob.tier, hash, blob.bytes.len() as u64))
            .collect(),
    )
}

// ─── Envelope + request mapping ───────────────────────────────────────────────

/// Serialize a wire enum (`Action`, `KeyMode`, …) to its bare protocol string.
fn wire_enum<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The [`ManifestEnvelope`] for one blob of `bundle`: every field of the head manifest,
/// verbatim, with `ciphertext_hash` naming **this** blob (the server's invariant-15
/// consistency rule — `manifest_envelope.ciphertext_hash` must equal the top-level `hash`).
#[must_use]
pub fn envelope_for(bundle: &UploadBundle, blob_hash: &str) -> ManifestEnvelope {
    ManifestEnvelope {
        crypto_suite_id: bundle.crypto_suite_id,
        protocol_version: bundle.protocol_version.clone(),
        album_id: Some(bundle.album_id.to_string()),
        file_id: bundle.asset_id.to_string(),
        amk_version: bundle.amk_version,
        ciphertext_hash: blob_hash.to_string(),
        plaintext_size: bundle.plaintext_size,
        chunk_size: bundle.chunk_size,
        key_mode: wire_enum(&bundle.key_mode),
        metadata_blob_hash: bundle.metadata_blob_hash.map(|h| h.to_hex()),
        created_by_user: bundle.created_by_user.to_string(),
        created_by_device: bundle.created_by_device.to_string(),
        client_version: bundle.client_version.clone(),
        timestamp: bundle.timestamp.clone(),
        action: wire_enum(&bundle.action),
        prior_provenance_hash: bundle.prior_provenance_hash.map(|h| h.to_hex()),
        retention_until: bundle.retention_until.clone(),
    }
}

/// The `POST /upload` body for one blob of `bundle`.
#[must_use]
pub fn create_request(
    bundle: &UploadBundle,
    blob: &BundleBlob<'_>,
    hash: &str,
) -> CreateUploadRequest {
    CreateUploadRequest {
        size: blob.bytes.len() as u64,
        hash: hash.to_string(),
        content_type: blob.content_type.to_string(),
        crypto_suite_id: bundle.crypto_suite_id,
        protocol_version: bundle.protocol_version.clone(),
        blob_role: blob.role,
        manifest_envelope: envelope_for(bundle, hash),
        album_id: Some(bundle.album_id.to_string()),
        owner_id: None,
        intent_id: None,
    }
}

// ─── Provisioning the album ───────────────────────────────────────────────────

/// Register `album_id` with the server before any blob session opens for it (slice `S-C25`).
///
/// The album's id is derived from the account master key, so the client knows it and the
/// server does not. [Invariant 6] refuses an upload whose album does not exist or is not
/// writable by the caller, so without this step a real push cannot land a single byte — the
/// ladder below would open its first session straight into `error.upload.album_access_denied`.
///
/// **Idempotent, and relied upon to be.** Provisioning an album the caller already owns is a
/// success that writes nothing, so this runs unconditionally on every push: there is no
/// client-side "already registered" flag to keep in sync across devices, which is exactly what
/// deriving the id from the master key buys. Pushing twice therefore cannot fail here.
///
/// No album name crosses the wire — album titles live in the encrypted sidecar.
///
/// [Invariant 6]: https://docs/design/threat-model/validation/#server-side-validation-invariants
#[instrument(skip(client), fields(album_id = %album_id))]
pub async fn ensure_album(
    client: &AlbumClient,
    album_id: uuid::Uuid,
) -> Result<ProvisionedAlbum, AlbumError> {
    let provisioned = client.provision(album_id).await?;
    tracing::info!(
        created = provisioned.created,
        "push: album provisioned on the server"
    );
    Ok(provisioned)
}

// ─── Driving a push ───────────────────────────────────────────────────────────

/// What a push **would** do for one bundle: the blobs it plans to open sessions for, in ladder
/// order, plus what it skipped and why. Computed without touching the network — the dry-run
/// primitive, and the first half of [`push_bundle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPlan {
    /// The blobs whose sessions would open now, in ladder order.
    pub blobs: Vec<TierBlob>,
    /// Blobs the server already holds (skipped from server truth, before any request).
    pub already_held: usize,
    /// Blobs a staged policy defers to a later window.
    pub deferred: usize,
    /// Bytes the planned blobs carry.
    pub bytes: u64,
}

/// Plan a push for one bundle: prune to what server truth says is outstanding, then let the
/// scheduler decide which of those tiers open now.
///
/// `force` ignores `held` and plans the whole ladder — the server still answers
/// `duplicate_blob` for anything it holds, which resolves as a merge.
#[must_use]
pub fn plan<H: std::hash::BuildHasher>(
    scheduler: &StagedScheduler,
    bundle: &UploadBundle,
    held: &HashSet<String, H>,
    force: bool,
) -> PushPlan {
    let asset = staged_asset(bundle);
    let total = asset.blobs.len();
    let outstanding = if force {
        asset
    } else {
        remaining_tiers(&asset, held)
    };
    let blobs = scheduler.plan_sessions(&outstanding);
    PushPlan {
        already_held: total - outstanding.blobs.len(),
        deferred: outstanding.blobs.len() - blobs.len(),
        bytes: blobs.iter().map(|b| b.size).sum(),
        blobs,
    }
}

/// Errors a push can fail with.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// The bundle's album could not be provisioned on the server, so nothing may be uploaded
    /// into it (invariant 6 would refuse every session).
    #[error(transparent)]
    Album(#[from] AlbumError),
    /// A blob's transfer failed non-recoverably.
    #[error(transparent)]
    Upload(#[from] UploadError),
    /// The scheduler planned a blob the bundle does not contain — an internal inconsistency
    /// between [`staged_asset`] and [`bundle_blobs`], surfaced rather than panicked on.
    #[error("asset {asset_id}: planned blob {hash} is not part of its upload bundle")]
    UnknownBlob {
        /// The asset whose plan named the blob.
        asset_id: String,
        /// The content address that could not be resolved.
        hash: String,
    },
}

/// How one blob of a bundle resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedBlob {
    /// The ladder tier the blob belongs to.
    pub tier: UploadTier,
    /// The blob's content address.
    pub hash: String,
    /// Whether it transferred or merged onto an already-stored blob.
    pub outcome: TierSessionOutcome,
}

/// What pushing one asset did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetPushReport {
    /// The asset id, as a string.
    pub asset_id: String,
    /// The blobs whose sessions were opened this run, in ladder order.
    pub pushed: Vec<PushedBlob>,
    /// Blobs the server already held (skipped from server truth, before any request).
    pub already_held: usize,
    /// Blobs a staged policy deferred to a later window.
    pub deferred: usize,
    /// Total bytes handed to the upload client this run.
    pub bytes: u64,
}

impl AssetPushReport {
    /// The ladder sequence actually driven — the ordering proof.
    #[must_use]
    pub fn tier_sequence(&self) -> Vec<UploadTier> {
        self.pushed.iter().map(|b| b.tier).collect()
    }

    /// Whether this run moved nothing: every blob was already held or deferred.
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.pushed.is_empty()
    }
}

/// Push one [`UploadBundle`] through `client`, in the order `scheduler` permits.
///
/// `held` is the set of blob content addresses the server already holds (from
/// [`held_from_feed`](crate::staged::held_from_feed) over a fresh feed pull); those blobs are
/// skipped without a request. `force` ignores `held` and re-drives every planned blob — the
/// server still answers `duplicate_blob` for anything it holds, which resolves as a merge.
#[instrument(skip_all, fields(asset_id = %bundle.asset_id, album_id = %bundle.album_id, force))]
pub async fn push_bundle<H: std::hash::BuildHasher + Sync>(
    client: &UploadClient,
    scheduler: &StagedScheduler,
    bundle: &UploadBundle,
    held: &HashSet<String, H>,
    force: bool,
) -> Result<AssetPushReport, PushError> {
    let asset_id = bundle.asset_id.to_string();
    let planned = plan(scheduler, bundle, held, force);

    let mut report = AssetPushReport {
        asset_id: asset_id.clone(),
        already_held: planned.already_held,
        deferred: planned.deferred,
        ..Default::default()
    };
    tracing::info!(
        planned = planned.blobs.len(),
        already_held = planned.already_held,
        deferred = planned.deferred,
        bytes = planned.bytes,
        "push: bundle planned"
    );

    let blobs = bundle_blobs(bundle);
    for tier_blob in planned.blobs {
        let Some((blob, hash)) = blobs.iter().find(|(_, hash)| *hash == tier_blob.hash) else {
            return Err(PushError::UnknownBlob {
                asset_id,
                hash: tier_blob.hash,
            });
        };
        let request = create_request(bundle, blob, hash);
        tracing::info!(
            tier = ?tier_blob.tier,
            role = ?blob.role,
            hash = %hash,
            bytes = blob.bytes.len(),
            content_type = %blob.content_type,
            "push: opening upload session"
        );
        let outcome = match client.upload(&request, blob.bytes).await? {
            UploadOutcome::Completed { session_id } => {
                tracing::info!(tier = ?tier_blob.tier, hash = %hash, %session_id, "push: blob transferred");
                TierSessionOutcome::Uploaded { session_id }
            }
            UploadOutcome::AlreadyStored { asset_ref } => {
                tracing::info!(tier = ?tier_blob.tier, hash = %hash, %asset_ref, "push: blob already stored — merged");
                TierSessionOutcome::AlreadyStored { asset_ref }
            }
        };
        report.bytes += blob.bytes.len() as u64;
        report.pushed.push(PushedBlob {
            tier: tier_blob.tier,
            hash: hash.clone(),
            outcome,
        });
    }

    tracing::info!(
        pushed = report.pushed.len(),
        bytes = report.bytes,
        "push: bundle complete"
    );
    Ok(report)
}

#[cfg(test)]
mod tests;
