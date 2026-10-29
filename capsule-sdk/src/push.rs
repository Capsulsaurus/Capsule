//! Pushing a locally-imported asset to a server — the glue between
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
//! addresses the server durably holds, and [`remaining_tiers`]
//! rebuilds exactly the outstanding work. Within a blob, `POST /upload`'s idempotent re-create
//! returns the authoritative offset; across an asset's blobs, a `duplicate_blob` answer is a
//! merge, not an error. Re-running a push against an unchanged library is therefore a no-op.
//!
//! # The index tier is two blobs, not one
//!
//! `T0` is **provenance and metadata**, in that order. The server publishes an asset to other
//! devices only once it holds both index-tier roles
//! (`capsule_server::upload::visibility::INDEX_TIER_ROLES`), and the head of the server-side
//! provenance chain — what a later lifecycle op's `prior_provenance_hash` must equal — is the
//! SHA-256 of the provenance blob's bytes. A ladder that shipped only the metadata blob
//! therefore uploaded an asset that no device could ever see and no op could ever chain onto:
//! the bytes were on the server and the asset was not in anybody's library.
//!
//! The provenance blob is the canonical CBOR of the chain's head `ProvenanceRecord`
//! ([`UploadBundle::provenance_blob`]), which `capsule-core` encodes once so the SDK and the
//! FFI cannot disagree about it. It goes **first** within `T0`: it is the blob the server reads
//! `prior_provenance_hash`, `action` and `retention_until` out of, so sending it first is the
//! order in which the asset's own claims arrive before the bytes they describe.
//!
//! **Every blob this module ships is ciphertext.** The original is re-derived from its
//! manifest's nonce prefix one upload chunk at a time ([`BlobBytes::Original`]), so a push holds
//! one chunk window of an original and never the whole file; the metadata blob is carried sealed, and **derivative blobs are
//! encrypted too** — `capsule-core` re-derives each one from the plaintext it holds locally
//! using the prefix that derivative's signed manifest recorded. Nothing here decrypts, encrypts,
//! or inspects a blob; it moves opaque bytes.
//!
//! **One deviation from "the envelope mirrors the signed manifest", and it is the server's
//! rule:** invariant 15 requires `manifest_envelope.ciphertext_hash == hash` (the top-level
//! declared content address of *this* blob). A bundle's metadata and derivative blobs are not
//! the original, so their envelope carries their own content address; every other field is the
//! head manifest's, verbatim. See [`envelope_for`].
//!
//! # A transfer is not custody
//!
//! Every byte reaching the server is not the same as the server holding the asset. A finalize
//! that lost its claim race answers the chunk `204` and is still running; an index failure after
//! the commit leaves bytes nobody references. So a push does not end at the last chunk:
//! [`verify_custody`] asks `POST /storage/verify` whether every blob the asset now relies on is
//! stored, indexed and retrievable, then fetches the asset's custody-receipt log and requires a
//! receipt per blob that verifies under the server's attestation-key history and names the
//! exact bytes, size and role the client sent. That history is the one the caller pinned for the
//! server (`capsule_library::library::pin_attestation_keys`), never a fetch trusted on its own. Only then is the asset reported as pushed, and
//! the verified receipts are what the caller persists beside the provenance chain.

use std::borrow::Cow;
use std::collections::HashSet;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::HybridVerifyingKey;
use capsule_core::crypto::provenance::DerivativeRole;
use capsule_library::import::UploadTier;
use capsule_library::library::{
    BlobRole as CoreBlobRole, CustodyReceipt, ReceiptExpectations, ReceiptRejection, verify_receipt,
};
use capsule_library::lifecycle::{OriginalCiphertext, UploadBundle};
use serde::Serialize;
use tracing::instrument;

use crate::albums::{AlbumClient, AlbumError, ProvisionedAlbum};
use crate::events::{TransferEvent, TransferEvents};
use crate::staged::{StagedAsset, StagedScheduler, TierBlob, TierSessionOutcome, remaining_tiers};
use crate::upload::{
    BlobRole, CreateUploadRequest, ManifestEnvelope, UploadClient, UploadError, UploadOutcome,
    UploadSource,
};
use crate::verify::{AssetQuery, StorageVerifyClient, VerifyError};

/// The content type a blob the server may not parse declares. The sealed metadata blob is AMK
/// ciphertext and the provenance blob is a signed CBOR document with no media type of its own —
/// the server's closed content-type enum (invariant 5) admits exactly this for both.
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
    /// The MIME type declared for this blob. For a derivative it is the derivative's signed
    /// `format`, which the envelope also declares as `derivative_format`.
    pub content_type: &'a str,
    /// The derivative's signed role, for a [`BlobRole::Derivative`] blob and for nothing else.
    pub derivative_role: Option<DerivativeRole>,
    /// The bytes to transfer.
    pub bytes: BlobBytes<'a>,
}

/// Where a [`BundleBlob`]'s bytes come from. Everything but the original is small and already
/// in the bundle; the original is a [`OriginalCiphertext`] source the upload client reads one
/// chunk window at a time, so a push never holds an original whole.
#[derive(Debug, Clone, Copy)]
pub enum BlobBytes<'a> {
    /// Bytes the bundle holds: the provenance and metadata blobs, derivative manifests and
    /// derivatives.
    Inline(&'a [u8]),
    /// The original's ciphertext, re-derived per window from the plaintext on disk.
    Original(&'a OriginalCiphertext),
}

impl BlobBytes<'_> {
    /// The blob's size in bytes — what `POST /upload` declares.
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Self::Inline(bytes) => bytes.len() as u64,
            Self::Original(original) => original.size(),
        }
    }

    /// The bytes, when the bundle holds them.
    #[must_use]
    pub fn inline(&self) -> Option<&[u8]> {
        match self {
            Self::Inline(bytes) => Some(bytes),
            Self::Original(_) => None,
        }
    }

    /// The whole blob in memory, for a caller that can only take a buffer (the FFI's
    /// `FfiUploadBlob`). An original read this way costs its full size; the SDK's own push
    /// never does it.
    pub fn to_vec(&self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Inline(bytes) => Ok(bytes.to_vec()),
            Self::Original(original) => original.to_vec(),
        }
    }
}

impl UploadSource for BlobBytes<'_> {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        match self {
            Self::Inline(bytes) => bytes.read_at(offset, len),
            Self::Original(original) => {
                let window = original.read_at(offset, len)?;
                Ok(Cow::Owned(window))
            }
        }
    }
}

/// Every transferable blob of `bundle`, in ladder order: the **index tier** (T0) — the
/// provenance blob then the sealed metadata blob — the **preview tier** (T1) — each derivative
/// tier's signed manifest, then each derivative — then the original (T2).
///
/// The provenance blob is unconditional: a managed asset always has a chain head, and the
/// server needs that blob both to publish the asset and to hold a chain head for the next
/// lifecycle op. The metadata blob is conditional — a head action that binds none (`delete`,
/// `trash-restore`, …) simply contributes no metadata rung, because the ladder is whatever the
/// manifest actually commits to and never a fabrication.
#[must_use]
pub fn bundle_blobs(bundle: &UploadBundle) -> Vec<(BundleBlob<'_>, String)> {
    let mut blobs =
        Vec::with_capacity(3 + bundle.derivative_manifests.len() + bundle.derivatives.len());
    blobs.push((
        BundleBlob {
            tier: UploadTier::Index,
            role: BlobRole::Provenance,
            content_type: OPAQUE_CONTENT_TYPE,
            derivative_role: None,
            bytes: BlobBytes::Inline(&bundle.provenance_blob),
        },
        // The content address of the provenance blob is `record_hash()` by definition — the
        // digest of the canonical record — so it is derived from the bytes rather than read off
        // a field. Nothing signs it: it *is* the chain head, and the chain is what signs.
        hash_bytes(&bundle.provenance_blob).to_hex(),
    ));
    if let Some(hash) = &bundle.metadata_blob_hash {
        blobs.push((
            BundleBlob {
                tier: UploadTier::Index,
                role: BlobRole::Metadata,
                content_type: OPAQUE_CONTENT_TYPE,
                derivative_role: None,
                bytes: BlobBytes::Inline(&bundle.metadata_blob),
            },
            hash.to_hex(),
        ));
    }
    // Each derivative tier's signed manifest, ahead of the derivatives: it is what a receiver
    // opens them with, so a device that sees a derivative has already been handed its manifest.
    for manifest in &bundle.derivative_manifests {
        blobs.push((
            BundleBlob {
                tier: UploadTier::Preview,
                role: BlobRole::DerivativeManifest,
                content_type: OPAQUE_CONTENT_TYPE,
                derivative_role: None,
                bytes: BlobBytes::Inline(manifest),
            },
            // Signed CBOR, not ciphertext, so — as for the provenance blob — its address is
            // derived from the bytes rather than read off a field.
            hash_bytes(manifest).to_hex(),
        ));
    }
    for derivative in &bundle.derivatives {
        blobs.push((
            BundleBlob {
                tier: UploadTier::Preview,
                role: BlobRole::Derivative,
                content_type: &derivative.format,
                derivative_role: Some(derivative.role),
                bytes: BlobBytes::Inline(&derivative.bytes),
            },
            derivative.ciphertext_hash.to_hex(),
        ));
    }
    blobs.push((
        BundleBlob {
            tier: UploadTier::Original,
            role: BlobRole::Original,
            content_type: &bundle.content_type,
            derivative_role: None,
            bytes: BlobBytes::Original(&bundle.original),
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
            .map(|(blob, hash)| TierBlob::new(blob.tier, hash, blob.bytes.size()))
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
        // Per-blob, not per-head: [`create_request`] fills the pair for a derivative.
        derivative_role: None,
        derivative_format: None,
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
///
/// A derivative's envelope declares its signed `(role, format)` pair, which the server checks
/// against the closed still-derivative format set on receipt; every other blob declares none,
/// because the server refuses the pair on anything but a derivative.
#[must_use]
pub fn create_request(
    bundle: &UploadBundle,
    blob: &BundleBlob<'_>,
    hash: &str,
) -> CreateUploadRequest {
    let mut manifest_envelope = envelope_for(bundle, hash);
    if let Some(role) = blob.derivative_role {
        manifest_envelope.derivative_role = Some(wire_enum(&role));
        manifest_envelope.derivative_format = Some(blob.content_type.to_string());
    }
    CreateUploadRequest {
        size: blob.bytes.size(),
        hash: hash.to_string(),
        content_type: blob.content_type.to_string(),
        crypto_suite_id: bundle.crypto_suite_id,
        protocol_version: bundle.protocol_version.clone(),
        blob_role: blob.role,
        manifest_envelope,
        album_id: Some(bundle.album_id.to_string()),
        owner_id: None,
        intent_id: None,
    }
}

// ─── Provisioning the album ───────────────────────────────────────────────────

/// Register `album_id` with the server before any blob session opens for it.
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
    /// The content addresses of those deferred blobs.
    pub deferred_hashes: Vec<String>,
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
    let deferred_hashes = outstanding
        .blobs
        .iter()
        .filter(|candidate| !blobs.iter().any(|b| b.hash == candidate.hash))
        .map(|candidate| candidate.hash.clone())
        .collect();
    PushPlan {
        already_held: total - outstanding.blobs.len(),
        deferred: outstanding.blobs.len() - blobs.len(),
        deferred_hashes,
        bytes: blobs.iter().map(|b| b.size).sum(),
        blobs,
    }
}

/// Every blob of the bundle except the deferred ones, in ladder order — what the server must
/// hold once a run that deferred `deferred` is done.
fn custody_hashes(blobs: &[(BundleBlob<'_>, String)], deferred: &[String]) -> Vec<String> {
    blobs
        .iter()
        .map(|(_, hash)| hash)
        .filter(|hash| !deferred.contains(hash))
        .cloned()
        .collect()
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
    /// The server refused a **create**'s provenance blob at finalization
    /// (`error.upload.envelope_rejected`), so the asset was not published (issue #822).
    ///
    /// The provenance blob is the first rung of the ladder and the one that publishes the
    /// manifest, so a refusal of it means nothing of the asset reached the row. The one cause a
    /// client can act on is invariant 18: the album's epoch moved past the one the create is
    /// sealed under. The wire answer is generic, so the caller decides — by checking whether it
    /// holds a newer epoch — whether a re-seal at the album's current epoch
    /// (`Workspace::reseal_create`) is the fix.
    #[error(
        "asset {asset_id}: the server refused its create, sealed at album epoch {amk_version}: \
         {source}"
    )]
    CreateRefused {
        /// The asset whose create was refused.
        asset_id: String,
        /// The album epoch the refused manifest names.
        amk_version: u32,
        /// The server's refusal, boxed to keep `PushError` small.
        #[source]
        source: Box<UploadError>,
    },
    /// The scheduler planned a blob the bundle does not contain — an internal inconsistency
    /// between [`staged_asset`] and [`bundle_blobs`], surfaced rather than panicked on.
    #[error("asset {asset_id}: planned blob {hash} is not part of its upload bundle")]
    UnknownBlob {
        /// The asset whose plan named the blob.
        asset_id: String,
        /// The content address that could not be resolved.
        hash: String,
    },
    /// The storage-verification or custody-receipt surface could not answer, so custody is
    /// unconfirmed — never assumed.
    #[error(transparent)]
    Verify(#[from] VerifyError),
    /// The server's verdict says it does not durably hold every blob the asset relies on.
    #[error("asset {asset_id}: the server does not durably hold {} of its blobs", not_durable.len())]
    NotDurable {
        /// The asset the verdict is for.
        asset_id: String,
        /// The content addresses not stored ∧ indexed ∧ retrievable (or absent from the verdict).
        not_durable: Vec<String>,
    },
    /// The asset's receipt log holds no receipt for a blob the asset relies on.
    #[error("asset {asset_id}: no custody receipt for blob {hash}")]
    ReceiptMissing {
        /// The asset whose log was read.
        asset_id: String,
        /// The blob with no receipt.
        hash: String,
    },
    /// Every receipt the log holds for a blob was refused: a signature no published attestation
    /// key verifies, or a field that disagrees with what the client sent.
    #[error("asset {asset_id}: the custody receipt for blob {hash} did not verify ({rejection:?})")]
    ReceiptRejected {
        /// The asset whose log was read.
        asset_id: String,
        /// The blob whose receipt was refused.
        hash: String,
        /// Why the last candidate receipt was refused.
        rejection: ReceiptRejection,
    },
}

impl PushError {
    /// A stable, machine-readable name for the failure — the `kind` a `--json` report carries.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Album(_) => "album",
            Self::Upload(_) => "upload",
            Self::CreateRefused { .. } => "create_refused",
            Self::UnknownBlob { .. } => "unknown_blob",
            Self::Verify(_) => "verify_unavailable",
            Self::NotDurable { .. } => "not_durable",
            Self::ReceiptMissing { .. } => "receipt_missing",
            Self::ReceiptRejected { .. } => "receipt_rejected",
        }
    }

    /// The stable `error.*` code the server supplied, when there was one.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Upload(UploadError::Rejected { code, .. }) => code.as_deref(),
            Self::CreateRefused { source, .. } => match source.as_ref() {
                UploadError::Rejected { code, .. } => code.as_deref(),
                _ => None,
            },
            Self::Upload(UploadError::UpgradeRequired { .. }) => {
                Some(capsule_i18n::error_codes::PROTOCOL_VERSION_UNSUPPORTED)
            }
            Self::Verify(error) => error.error_code(),
            Self::Album(error) => error.error_code(),
            _ => None,
        }
    }
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
    /// The content addresses the server must hold once this run is done, in ladder order:
    /// every blob of the bundle except those a staged policy deferred. This is the set
    /// [`verify_custody`] declares to `/storage/verify` and requires a receipt for — a deferred
    /// tier was never sent, so it is not yet the server's to hold.
    pub custody_hashes: Vec<String>,
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
///
/// Every blob's transfer steps are published to `client`'s
/// [`events`](UploadClient::with_events) handle, tagged with the blob's content address.
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
    let blobs = bundle_blobs(bundle);

    let mut report = AssetPushReport {
        asset_id: asset_id.clone(),
        already_held: planned.already_held,
        deferred: planned.deferred,
        custody_hashes: custody_hashes(&blobs, &planned.deferred_hashes),
        ..Default::default()
    };
    tracing::info!(
        planned = planned.blobs.len(),
        already_held = planned.already_held,
        deferred = planned.deferred,
        bytes = planned.bytes,
        "push: bundle planned"
    );

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
            bytes = blob.bytes.size(),
            content_type = %blob.content_type,
            "push: opening upload session"
        );
        let uploaded = client
            .upload(&request, &blob.bytes)
            .await
            .map_err(|error| classify_refusal(bundle, blob, error))?;
        let outcome = match uploaded {
            UploadOutcome::Completed { session_id } => {
                tracing::info!(tier = ?tier_blob.tier, hash = %hash, %session_id, "push: blob transferred");
                TierSessionOutcome::Uploaded { session_id }
            }
            UploadOutcome::AlreadyStored { asset_ref } => {
                tracing::info!(tier = ?tier_blob.tier, hash = %hash, %asset_ref, "push: blob already stored — merged");
                TierSessionOutcome::AlreadyStored { asset_ref }
            }
        };
        report.bytes += blob.bytes.size();
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

/// Name a transfer failure [`PushError::CreateRefused`] when it is the one a stale-epoch create
/// meets: the server's finalization refused the **provenance** blob of a **create** bundle
/// (`error.upload.envelope_rejected`). Every other failure stays [`PushError::Upload`].
///
/// Decided here, where the blob's role is known, because the wire answer is generic: the same
/// code covers every re-validation a finalization runs, and it is the role that rules out the one
/// unsafe reading. A refusal on any later rung could follow a provenance blob that did land, and
/// re-sealing an asset the server already published would fork its chain.
fn classify_refusal(bundle: &UploadBundle, blob: &BundleBlob<'_>, error: UploadError) -> PushError {
    let refused_at_finalize = matches!(
        &error,
        UploadError::Rejected { code: Some(code), .. }
            if code == capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED
    );
    let is_genesis = bundle.action == capsule_core::crypto::provenance::Action::Create
        && bundle.prior_provenance_hash.is_none();
    if refused_at_finalize && is_genesis && blob.role == BlobRole::Provenance {
        tracing::warn!(
            amk_version = bundle.amk_version,
            "push: the server refused this create's manifest at finalization; \
             nothing of it is published"
        );
        return PushError::CreateRefused {
            asset_id: bundle.asset_id.to_string(),
            amk_version: bundle.amk_version,
            source: Box::new(error),
        };
    }
    PushError::Upload(error)
}

// ─── Redacting a deleted asset ────────────────────────────────────────────────

/// What [`redact_asset`] did to one asset's open upload sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redaction {
    /// The sessions cancelled (`DELETE /upload/{id}` answered `204` or `404`): no further byte
    /// of them is sent, and the server dropped what they had staged.
    pub cancelled: Vec<String>,
    /// The sessions the server would not cancel because their finalization was already running
    /// or done (`409 error.upload.session_not_active`). Their blob lands; the asset's `delete`
    /// record, sent after, supersedes the create it completes.
    pub finalizing: Vec<String>,
}

/// Abort every open upload session this account holds for `asset_id`: the network half of
/// redacting an asset deleted before its upload finished
/// ([Organization — Deletion State Machine]).
///
/// The sessions come from `GET /upload/sessions`, filtered by the asset each names, so a
/// session a previous run left behind (a staged tier that never finished, a push killed
/// mid-ladder) is found without any client-side state. Each is cancelled with
/// `DELETE /upload/{id}`:
///
/// - `204` or `404` is a cancelled session: its staged bytes and quota reservation are released.
/// - `409 error.upload.session_not_active` is the race with a finalization that started before
///   the abort reached it. Finalization is not interruptible, so that blob lands; the session is
///   reported in [`Redaction::finalizing`] and the caller's `delete` supersedes the create.
///
/// Any other answer fails the redaction. Re-running it is safe: a session already cancelled is a
/// `404`, and the listing no longer names it.
///
/// Pending tiers need no call: a deferred tier has no session, and a push of a trashed asset
/// plans nothing, so the abort is the whole of what the server has to forget.
///
/// [Organization — Deletion State Machine]: https://docs/design/organization/#deletion-state-machine
#[instrument(skip(client), fields(%asset_id))]
pub async fn redact_asset(client: &UploadClient, asset_id: &str) -> Result<Redaction, UploadError> {
    let sessions = client.list_sessions().await?;
    let mut redaction = Redaction::default();
    for session in sessions.iter().filter(|s| s.asset_id == asset_id) {
        match client.delete(&session.id).await {
            Ok(()) => {
                tracing::info!(session_id = %session.id, status = %session.status, received = session.received_bytes, "redact: upload session cancelled");
                redaction.cancelled.push(session.id.clone());
            }
            Err(UploadError::Rejected {
                status: 409,
                code: Some(code),
                ..
            }) if code == capsule_i18n::error_codes::UPLOAD_SESSION_NOT_ACTIVE => {
                tracing::info!(session_id = %session.id, status = %session.status, "redact: the session is already finalizing; its delete will supersede it");
                redaction.finalizing.push(session.id.clone());
            }
            Err(error) => {
                tracing::warn!(session_id = %session.id, %error, "redact: an upload session could not be cancelled");
                return Err(error);
            }
        }
    }
    tracing::info!(
        listed = sessions.len(),
        cancelled = redaction.cancelled.len(),
        finalizing = redaction.finalizing.len(),
        "redact: the asset's open upload sessions are aborted"
    );
    Ok(redaction)
}

// ─── Confirming custody ───────────────────────────────────────────────────────

/// What [`verify_custody`] established for one asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCustody {
    /// The server's trusted clock when it judged the asset durable (RFC 3339).
    pub checked_at: String,
    /// One verified receipt per blob the asset relies on, in ladder order — the evidence the
    /// caller persists beside the provenance chain
    /// ([`append_receipt`](capsule_library::library::append_receipt)).
    pub receipts: Vec<CustodyReceipt>,
}

/// The core receipt role for a protocol blob role, where the core's closed enum names it.
fn core_role(role: BlobRole) -> Option<CoreBlobRole> {
    match role {
        BlobRole::Original => Some(CoreBlobRole::Original),
        BlobRole::Derivative => Some(CoreBlobRole::Derivative),
        BlobRole::Metadata => Some(CoreBlobRole::Metadata),
        BlobRole::Provenance => Some(CoreBlobRole::Provenance),
        BlobRole::Backup | BlobRole::DerivativeManifest => None,
    }
}

/// Check one receipt against the blob the client sent.
///
/// The four roles `capsule_core`'s receipt verifier names go through
/// [`verify_receipt`] unchanged. A `derivative_manifest` blob has no arm there, so it gets the
/// same signature and field checks spelled against its wire role; the `received_at` drift bound
/// is the one check it does not get, and that bound is evidentiary only.
fn check_receipt(
    receipt: &CustodyReceipt,
    keys: &[HybridVerifyingKey],
    blob: &BundleBlob<'_>,
    hash: Hash32,
    now_unix: i64,
) -> Result<(), ReceiptRejection> {
    let size = blob.bytes.size();
    // The chain position, and only on the blob that *is* the manifest. SHA-256 by definition,
    // which is what `hash_bytes` computes, whatever digest the suite addresses blobs with.
    let envelope_hash = (blob.role == BlobRole::Provenance)
        .then(|| blob.bytes.inline().map(hash_bytes))
        .flatten();
    if let Some(role) = core_role(blob.role) {
        let expected = ReceiptExpectations {
            ciphertext_hash: hash,
            size,
            role,
            envelope_hash,
        };
        return verify_receipt(receipt, keys, &expected, now_unix);
    }
    if !keys.iter().any(|key| receipt.verify_under(key)) {
        return Err(ReceiptRejection::Signature);
    }
    let core = &receipt.core;
    if core.ciphertext_hash != hash {
        return Err(ReceiptRejection::FieldMismatch("ciphertext_hash"));
    }
    if core.size != size {
        return Err(ReceiptRejection::FieldMismatch("size"));
    }
    if core.blob_role != wire_enum(&blob.role) {
        return Err(ReceiptRejection::FieldMismatch("blob_role"));
    }
    if core.envelope_hash.is_some() {
        return Err(ReceiptRejection::FieldMismatch("envelope_hash"));
    }
    Ok(())
}

/// Confirm the server holds what [`push_bundle`] sent, and collect the evidence that it does.
///
/// Two facts, both required, in this order:
///
/// 1. **Durability.** `POST /storage/verify` over exactly `report.custody_hashes`. The verdict's
///    aggregate is not trusted on its own: every declared hash must appear in the per-blob
///    detail as stored ∧ indexed ∧ retrievable, the re-check
///    [`release_is_safe`](capsule_library::library::release_is_safe) makes before a release.
/// 2. **Accountability.** The asset's receipt log (`GET /assets/{id}/receipts`), and for every
///    declared blob a receipt that verifies under one of `attestation_keys` (the pinned
///    history, not a fetch the caller has not checked against its pin) and names the
///    bytes, size, role and — for the provenance blob — the chain position the client sent.
///
/// The durable log rather than the per-session fetch, because a blob this run did not
/// transfer — one the feed said was held, or one a retry of this same asset found finalized as
/// `duplicate_blob` — has no session to ask about, and its custody needs the same evidence as a
/// fresh one. A `duplicate_blob` naming a *different* asset is covered the same way: the server
/// adopts that blob onto this asset before refusing (#672), indexing it under this asset and
/// receipting it in this asset's log.
///
/// `now_unix` is the client's clock, injected for the receipt's drift bound.
pub async fn verify_custody(
    client: &StorageVerifyClient,
    attestation_keys: &[HybridVerifyingKey],
    bundle: &UploadBundle,
    report: &AssetPushReport,
    now_unix: i64,
) -> Result<VerifiedCustody, PushError> {
    verify_custody_with_events(
        client,
        attestation_keys,
        bundle,
        report,
        now_unix,
        &TransferEvents::disabled(),
    )
    .await
}

/// [`verify_custody`], publishing the verdict ([`TransferEvent::Verify`]) and each verified
/// receipt ([`TransferEvent::Receipt`]) to `events`.
#[instrument(skip_all, fields(asset_id = %bundle.asset_id, blobs = report.custody_hashes.len()))]
pub async fn verify_custody_with_events(
    client: &StorageVerifyClient,
    attestation_keys: &[HybridVerifyingKey],
    bundle: &UploadBundle,
    report: &AssetPushReport,
    now_unix: i64,
    events: &TransferEvents,
) -> Result<VerifiedCustody, PushError> {
    let asset_id = bundle.asset_id.to_string();
    let blobs = bundle_blobs(bundle);
    let mut declared = Vec::with_capacity(report.custody_hashes.len());
    for hash in &report.custody_hashes {
        let Some((blob, _)) = blobs.iter().find(|(_, candidate)| candidate == hash) else {
            return Err(PushError::UnknownBlob {
                asset_id,
                hash: hash.clone(),
            });
        };
        let parsed = Hash32::from_hex(hash).map_err(|_| PushError::UnknownBlob {
            asset_id: asset_id.clone(),
            hash: hash.clone(),
        })?;
        declared.push((blob, hash, parsed));
    }

    // 1. Durability.
    let query = AssetQuery {
        asset_id: bundle.asset_id,
        blob_hashes: declared.iter().map(|(_, _, parsed)| *parsed).collect(),
    };
    let verdict = client
        .verify(&[query], false)
        .await?
        .into_iter()
        .find(|verdict| verdict.asset_id == bundle.asset_id)
        .ok_or_else(|| VerifyError::Malformed("the verdict omits the asset asked about".into()))?;
    let not_durable: Vec<String> = declared
        .iter()
        .filter(|(_, _, parsed)| {
            !verdict
                .blobs
                .iter()
                .any(|blob| blob.hash == *parsed && blob.safely_stored())
        })
        .map(|(_, hash, _)| (*hash).clone())
        .collect();
    events.emit(|| TransferEvent::Verify {
        asset_id: asset_id.clone(),
        durable: verdict.durable,
        blobs: declared.len() as u64,
        not_durable: not_durable.len() as u64,
    });
    if !verdict.durable || !not_durable.is_empty() {
        tracing::warn!(
            durable = verdict.durable,
            not_durable = ?not_durable,
            "push: the server does not durably hold the asset"
        );
        return Err(PushError::NotDurable {
            asset_id,
            not_durable,
        });
    }
    tracing::debug!(checked_at = %verdict.checked_at, "push: the server reports the asset durable");

    // 2. Accountability.
    let log = client.fetch_asset_receipts(bundle.asset_id).await?;
    tracing::debug!(
        receipts = log.len(),
        "push: the asset's receipt log fetched"
    );
    let mut receipts = Vec::with_capacity(declared.len());
    for (blob, hash, parsed) in declared {
        let mut last_rejection = None;
        let mut verified = None;
        for candidate in log.iter().filter(|r| r.core.ciphertext_hash == parsed) {
            match check_receipt(candidate, attestation_keys, blob, parsed, now_unix) {
                Ok(()) => {
                    verified = Some(candidate.clone());
                    break;
                }
                Err(rejection) => last_rejection = Some(rejection),
            }
        }
        match (verified, last_rejection) {
            (Some(receipt), _) => {
                tracing::debug!(
                    hash = %hash,
                    role = ?blob.role,
                    receipt_seq = receipt.core.receipt_seq,
                    "push: custody receipt verified"
                );
                events.emit(|| TransferEvent::Receipt {
                    asset_id: asset_id.clone(),
                    blob: hash.clone(),
                    receipt_seq: receipt.core.receipt_seq,
                });
                receipts.push(receipt);
            }
            (None, Some(rejection)) => {
                tracing::warn!(hash = %hash, ?rejection, "push: custody receipt refused");
                return Err(PushError::ReceiptRejected {
                    asset_id,
                    hash: hash.clone(),
                    rejection,
                });
            }
            (None, None) => {
                tracing::warn!(hash = %hash, "push: no custody receipt for a blob");
                return Err(PushError::ReceiptMissing {
                    asset_id,
                    hash: hash.clone(),
                });
            }
        }
    }

    tracing::info!(receipts = receipts.len(), "push: custody verified");
    Ok(VerifiedCustody {
        checked_at: verdict.checked_at,
        receipts,
    })
}

#[cfg(test)]
mod tests;
