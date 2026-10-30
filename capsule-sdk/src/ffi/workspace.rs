//! The **workspace verbs** of the `capsule_sdk` uniffi namespace — enroll,
//! create an album, seal + import an asset, verify, sync-apply, mint an escrow, publish a
//! directory, label regions, stacks and albums, move assets through the trash — over
//! `capsule-core`'s [`Workspace`].
//!
//! # What this layer is, and is not
//!
//! It is **orchestration and shape**: it locks one `Workspace`, converts ids and blobs at the
//! boundary, and flattens core's rich enums into `Ffi*` mirrors an app can switch on. Every
//! cryptographic step — key derivation, STREAM encryption, manifest signing, the
//! [`verify_asset`] chokepoint, metadata sealing and opening, escrow wrapping — happens inside
//! `capsule-core`, reached through exactly one call per verb. There is no second implementation
//! of any of it here, and adding one would be a defect rather than an optimization.
//!
//! # Core's seams, declared in this namespace
//!
//! `capsule-core` exports no uniffi namespace (ADR-0005): its hardware seams are plain Rust
//! traits. So the seams a native element implements are declared here in *this* namespace —
//! [`FfiHardwareSigner`], [`FfiHardwareKeyAgreement`] and [`FfiHardwareSignerError`] mirror
//! core's `HardwareSigner`, `HardwareKeyAgreement` and `HardwareSignerError`, and
//! `ForeignHardwareSigner` / `ForeignKeyAgreement` adapt one to the other. They are declarations of the same contract,
//! not a second implementation of it: not a byte of signing or key-agreement logic lives here.
//!
//! # Transport independence
//!
//! Every verb below is `capsule-core`-facing. None of them names the SDK's network stack, so
//! the pending re-fronting of the sync half on REST changes nothing in this file's signatures:
//! the bytes a feed entry carries are the same bytes whatever delivered them, and
//! [`FfiWorkspace::upload_blobs`] hands back requests that
//! [`FfiSession::upload`](crate::ffi::FfiSession::upload) drives regardless of what is
//! underneath it.
//!
//! [`verify_asset`]: capsule_core::crypto::verify_asset::verify_asset
//! [`Workspace`]: capsule_library::lifecycle::Workspace

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{
    HardwareBackedSigner, HardwareKeyAgreement, HardwareSigner, HardwareSignerError, P256HybridDek,
    Signer, encapsulate_to_p256_public,
};
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::provenance::ProvenanceRecord;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::sidecar::{
    LabelError, Region, RegionError, RegionShape, SidecarV1, normalize_label,
};
use capsule_library::lifecycle::{
    FeedDrain, HardwareSignerBinding, LifecycleError, QuarantineReason, RemoteAssetFacts,
    RemoteEntry, RemoteHead, RemoteOriginal, SyncApplyOutcome, TrashEntry, UploadBundle, Workspace,
    hash_ciphertext_from,
};
use jiff::Timestamp;
use uuid::Uuid;

use super::{FfiError, FfiUploadRequest};
use crate::upload::UploadError;

// ─── Hardware-signer seam (this namespace's declaration of core's contract) ───

/// Failure surfaced by a foreign [`FfiHardwareSigner`]. Mirrors `capsule-core`'s
/// `HardwareSignerError` variant for variant — it is the same contract, declared in the
/// `capsule_sdk` namespace because core's own type carries no uniffi derive.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiHardwareSignerError {
    /// The user cancelled the biometric / the element refused authentication.
    #[error("hardware authentication cancelled")]
    AuthCancelled,
    /// No secure element is available on this device.
    #[error("hardware secure element unavailable")]
    Unavailable,
    /// No key exists for the requested alias.
    #[error("hardware key not found")]
    NotFound,
    /// The private key was readable — non-exportability is violated (a failure).
    #[error("hardware private key is exportable")]
    Exportable,
    /// Any other backend error.
    #[error("hardware backend error: {0}")]
    Backend(String),
}

impl From<FfiHardwareSignerError> for HardwareSignerError {
    fn from(err: FfiHardwareSignerError) -> Self {
        match err {
            FfiHardwareSignerError::AuthCancelled => Self::AuthCancelled,
            FfiHardwareSignerError::Unavailable => Self::Unavailable,
            FfiHardwareSignerError::NotFound => Self::NotFound,
            FfiHardwareSignerError::Exportable => Self::Exportable,
            FfiHardwareSignerError::Backend(detail) => Self::Backend(detail),
        }
    }
}

/// The per-platform secure element (Secure Enclave, StrongBox, TPM), implemented by native
/// Swift/Kotlin over the uniffi foreign-trait boundary.
///
/// The byte shapes are fixed by the constructor the element is handed to, not by the trait:
///
/// - [`FfiWorkspace::create_with_p256_hardware_signer`] and
///   [`FfiWorkspace::open_with_p256_hardware_signer`] — the **P-256** shape shipping
///   elements actually provide: [`enroll`](Self::enroll) /
///   [`classical_public_key`](Self::classical_public_key) return a SEC1 public key and
///   [`sign_classical`](Self::sign_classical) a **DER-encoded ECDSA** signature over
///   `SHA-256(msg)`.
/// - [`FfiWorkspace::create_with_hardware_signer`] — the **Ed25519** shape: a 32-byte public
///   key and a 64-byte signature over `msg`. No shipping secure element holds Ed25519 keys, so
///   this is the software/TPM reference path.
///
/// Either way Rust composes the classical half with a software-sealed ML-DSA-65 half into the
/// published hybrid device key.
#[uniffi::export(with_foreign)]
pub trait FfiHardwareSigner: Send + Sync {
    /// Generate (or bind to) the hardware keypair for `key_alias`, returning its public key in
    /// the constructor's shape: a SEC1 P-256 key for
    /// [`FfiWorkspace::create_with_p256_hardware_signer`], a 32-byte Ed25519 key for
    /// [`FfiWorkspace::create_with_hardware_signer`]. Idempotent per alias.
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError>;

    /// The public key for an already-enrolled `key_alias`, in the same constructor-dependent
    /// shape [`enroll`](Self::enroll) returns (SEC1 P-256, or 32-byte Ed25519).
    fn classical_public_key(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError>;

    /// Sign `msg` with the hardware key for `key_alias`, in the constructor's shape: a
    /// DER-encoded ECDSA signature over `SHA-256(msg)` for
    /// [`FfiWorkspace::create_with_p256_hardware_signer`], a 64-byte Ed25519 signature over
    /// `msg` for [`FfiWorkspace::create_with_hardware_signer`].
    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, FfiHardwareSignerError>;

    /// Non-exportability assertion: a conforming element MUST refuse to reveal the private
    /// bytes, returning [`FfiHardwareSignerError::Exportable`] if they can be read.
    fn assert_non_exportable(&self, key_alias: String) -> Result<(), FfiHardwareSignerError>;
}

/// Adapts a foreign [`FfiHardwareSigner`] to `capsule-core`'s `HardwareSigner`, so the core
/// composition (`P256HybridSigningKey::enroll`) drives the native element unchanged. Pure
/// delegation — every call forwards, and only the error type is translated.
struct ForeignHardwareSigner(Arc<dyn FfiHardwareSigner>);

impl HardwareSigner for ForeignHardwareSigner {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.0.enroll(key_alias).map_err(Into::into)
    }
    fn classical_public_key(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.0.classical_public_key(key_alias).map_err(Into::into)
    }
    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HardwareSignerError> {
        self.0.sign_classical(key_alias, msg).map_err(Into::into)
    }
    fn assert_non_exportable(&self, key_alias: String) -> Result<(), HardwareSignerError> {
        self.0.assert_non_exportable(key_alias).map_err(Into::into)
    }
}

/// The per-platform **key-agreement** element (Secure Enclave `P256.KeyAgreement`, StrongBox
/// ECDH, TPM 2.0 ECDH), implemented by native Swift/Kotlin over the uniffi foreign-trait
/// boundary — the ECDH analogue of [`FfiHardwareSigner`], and this namespace's declaration of
/// `capsule-core`'s `HardwareKeyAgreement` contract.
///
/// It backs the hardware-bound classical half of the device **encryption** key:
/// the element holds a static P-256 private key and performs the ECDH, so the scalar never
/// leaves hardware, while Rust drives the software-sealed ML-KEM-768 half and the hybrid
/// combiner. Failures reuse [`FfiHardwareSignerError`], exactly as core's trait reuses its
/// signer error.
#[uniffi::export(with_foreign)]
pub trait FfiHardwareKeyAgreement: Send + Sync {
    /// Generate (or bind to) the hardware P-256 key-agreement keypair for `key_alias`,
    /// returning its public key as **uncompressed SEC1** (`0x04‖x‖y`, 65 bytes). Idempotent per
    /// alias.
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError>;

    /// The uncompressed-SEC1 P-256 public key for an already-enrolled `key_alias`.
    fn public_key(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError>;

    /// ECDH against `peer_public` (the sender's ephemeral P-256 public key, uncompressed SEC1)
    /// with the hardware key for `key_alias`, returning the **raw 32-byte shared secret** (the
    /// big-endian x-coordinate, before any KDF).
    fn key_agreement(
        &self,
        key_alias: String,
        peer_public: Vec<u8>,
    ) -> Result<Vec<u8>, FfiHardwareSignerError>;

    /// Non-exportability assertion: a conforming element MUST refuse to reveal the private
    /// bytes, returning [`FfiHardwareSignerError::Exportable`] if they can be read.
    fn assert_non_exportable(&self, key_alias: String) -> Result<(), FfiHardwareSignerError>;
}

/// Adapts a foreign [`FfiHardwareKeyAgreement`] to `capsule-core`'s `HardwareKeyAgreement`, so
/// the core composition (`P256HybridDek::enroll`) drives the native element unchanged. Pure
/// delegation — only the error type is translated.
struct ForeignKeyAgreement(Arc<dyn FfiHardwareKeyAgreement>);

impl HardwareKeyAgreement for ForeignKeyAgreement {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.0.enroll(key_alias).map_err(Into::into)
    }
    fn public_key(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.0.public_key(key_alias).map_err(Into::into)
    }
    fn key_agreement(
        &self,
        key_alias: String,
        peer_public: Vec<u8>,
    ) -> Result<Vec<u8>, HardwareSignerError> {
        self.0
            .key_agreement(key_alias, peer_public)
            .map_err(Into::into)
    }
    fn assert_non_exportable(&self, key_alias: String) -> Result<(), HardwareSignerError> {
        self.0.assert_non_exportable(key_alias).map_err(Into::into)
    }
}

/// Parse the 32-byte software-sealed post-quantum seed every hardware composition takes, as a
/// typed error rather than a panic at the boundary.
fn parse_ml_seed(bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    bytes.try_into().map_err(|_| FfiError::InvalidArgument {
        detail: "ml_seed must be exactly 32 bytes".into(),
    })
}

/// Smoke the **hardware-bound P-256 device encryption key** across the FFI seam:
/// enroll a key-agreement key in `hardware` under `key_alias`, publish the hybrid public key,
/// encapsulate a fresh shared secret to it **in software** (the sender side), then
/// **decapsulate through the element's ECDH**. Returns `true` iff the two secrets match —
/// proving the element's ECDH composes into the hybrid KEM without the private scalar leaving
/// hardware. `ml_seed` (32 bytes) is the software-sealed ML-KEM-768 half.
///
/// The DEK is not yet wired into workspace creation, so this focused round-trip is the
/// composition's on-device check — the DEK analogue of
/// [`FfiWorkspace::create_with_p256_hardware_signer`]. Every step is one `capsule-core` call.
#[uniffi::export]
pub fn p256_hardware_dek_round_trip(
    hardware: Arc<dyn FfiHardwareKeyAgreement>,
    key_alias: String,
    ml_seed: Vec<u8>,
) -> Result<bool, FfiError> {
    let seed = parse_ml_seed(&ml_seed)?;
    let dek = P256HybridDek::enroll(Arc::new(ForeignKeyAgreement(hardware)), key_alias, &seed)
        .map_err(|e| FfiError::Workspace {
            detail: format!("hardware P-256 DEK enrollment failed: {e}"),
        })?;
    let workspace_error = |e: capsule_core::crypto::CryptoError| FfiError::Workspace {
        detail: e.to_string(),
    };
    let (ciphertext, sender_secret) =
        encapsulate_to_p256_public(&dek.public_bytes()).map_err(workspace_error)?;
    let receiver_secret = dek.decapsulate(&ciphertext).map_err(workspace_error)?;
    let matched = sender_secret == receiver_secret;
    tracing::debug!(matched, "hardware P-256 DEK round trip");
    Ok(matched)
}

// ─── Small mirrors ───────────────────────────────────────────────────────────

/// Device hardware tier, selecting the Argon2id cost at wrap time. Mirrors core's `DeviceTier`
/// in this namespace (see the module docs on why it is not shared).
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiDeviceTier {
    /// ≤ 2 GiB total RAM (entry-level Android / embedded).
    LowRam,
    /// Default for phones and laptops.
    Normal,
    /// ≥ 8 GiB; used when wrapping new escrow blobs from a desktop.
    Desktop,
}

impl From<FfiDeviceTier> for DeviceTier {
    fn from(tier: FfiDeviceTier) -> Self {
        match tier {
            FfiDeviceTier::LowRam => Self::LowRam,
            FfiDeviceTier::Normal => Self::Normal,
            FfiDeviceTier::Desktop => Self::Desktop,
        }
    }
}

/// A foreign client's self-reported build identity: its product id and own semver.
/// The git commit + dirty flag come from the core this app links, together composing the
/// `client_id/semver+commit` `client_version` every manifest carries — so a defect in a shipped
/// app build is traceable across the assets it produced.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiClientBuild {
    /// The app's product id, e.g. `capsule-ios`.
    pub client_id: String,
    /// The app's own semantic version, e.g. `1.4.2`.
    pub semver: String,
}

/// One album this workspace holds key material for.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAlbum {
    /// Album id (UUID string).
    pub album_id: String,
    /// Display name, which lives only in the client's own state — it never crosses the wire.
    pub name: String,
}

/// The verification verdict, flattened for FFI (reasons as stable reason-code strings).
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiVerifyOutcome {
    /// The asset verified and may enter the trusted set.
    Accept,
    /// The asset is permanently rejected (quarantined); `reason` is the reason code.
    TerminalReject {
        /// Stable reason-code string (e.g. `RemovedWriter`).
        reason: String,
    },
    /// The asset is recoverable-pending (held + retried); `reason` is the reason code.
    Pending {
        /// Stable reason-code string (e.g. `AmkNotYetLocal`).
        reason: String,
    },
}

impl From<VerifyOutcome> for FfiVerifyOutcome {
    fn from(outcome: VerifyOutcome) -> Self {
        match outcome {
            VerifyOutcome::Accept => Self::Accept,
            VerifyOutcome::TerminalReject(reason) => Self::TerminalReject {
                reason: format!("{reason:?}"),
            },
            VerifyOutcome::Pending(reason) => Self::Pending {
                reason: format!("{reason:?}"),
            },
        }
    }
}

// ─── Seal + import → upload bridge ───────────────────────────────────────────

/// One transferable blob of a sealed asset, paired with the `POST /upload` body that declares
/// it. Hand [`request`](Self::request) and [`bytes`](Self::bytes) straight to
/// [`FfiSession::upload`](crate::ffi::FfiSession::upload) — this record exists so an app never
/// has to assemble a manifest envelope itself.
///
/// The blobs come back in **ladder order**: the metadata blob (T0 — the index tier that makes
/// the asset visible), then derivatives (T1), then the original (T2).
///
/// [`bytes`](Self::bytes) holds each blob whole, the original included, so an original costs
/// its full size in memory here. [`FfiSession::push_asset`](crate::ffi::FfiSession::push_asset)
/// pushes the same blobs with the original streamed, and is the path for an original of any
/// size.
#[derive(uniffi::Record)]
pub struct FfiUploadBlob {
    /// The ladder tier this blob occupies (`index` / `preview` / `original`).
    pub tier: String,
    /// The blob's content address (lowercase hex) — the value the server dedups on.
    pub hash: String,
    /// The `POST /upload` body for this blob, with the manifest envelope already projected.
    pub request: FfiUploadRequest,
    /// The bytes to transfer.
    pub bytes: Vec<u8>,
}

// ─── Sync-apply ──────────────────────────────────────────────────────────────

/// One remote feed entry to apply, as a sync feed delivers it.
///
/// `album_id` and the two id-bearing fields carry the feed's own **16-byte UUID** form (the
/// `bytes album_id` of `capsule.sync.v1.SyncEntry`), so an entry from
/// [`FfiSession::sync_pull`](crate::ffi::FfiSession::sync_pull) is passed through unchanged.
#[derive(Debug, uniffi::Record)]
pub struct FfiSyncEntry {
    /// The album the feed claims this entry belongs to — 16 raw UUID bytes. Cross-checked
    /// against the manifest by the chokepoint itself, never trusted on its own.
    pub album_id: Vec<u8>,
    /// The signed `AssetManifest` as opaque canonical CBOR, carried verbatim.
    pub manifest_cbor: Vec<u8>,
    /// The sealed metadata blob the manifest commits to.
    pub metadata_blob: Vec<u8>,
    /// The asset's original ciphertext. Required: the chokepoint's content-integrity step
    /// hashes it against the manifest's declared content address, and there is no variant of
    /// that check which skips it — so fetch the original blob before applying.
    ///
    /// It crosses whole, so this record costs the original's full size in memory. An original
    /// of any size is received with
    /// [`FfiSession::fetch_blob_to_path`](crate::ffi::FfiSession::fetch_blob_to_path), and
    /// applied and opened from that file with an [`FfiSyncHead`] through
    /// [`FfiWorkspace::apply_sync_entry_from_file`] and [`FfiWorkspace::open_synced_original`],
    /// in bounded memory.
    pub original_ciphertext: Vec<u8>,
    /// The 32-byte provenance head your catalog already holds for this asset, or `None` if it
    /// has never seen it. This is what decides replay and fork; feed back
    /// [`FfiAssetFacts::provenance_head`] after a successful apply.
    pub local_chain_head: Option<Vec<u8>>,
}

/// One remote feed entry whose original is **on disk** rather than in hand: an
/// [`FfiSyncEntry`] without its `original_ciphertext`, which
/// [`FfiWorkspace::apply_sync_entry_from_file`] and [`FfiWorkspace::open_synced_original`] read
/// from the file [`FfiSession::fetch_blob_to_path`](crate::ffi::FfiSession::fetch_blob_to_path)
/// produced, in memory bounded by one STREAM chunk whatever the original's size.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncHead {
    /// As [`FfiSyncEntry::album_id`].
    pub album_id: Vec<u8>,
    /// As [`FfiSyncEntry::manifest_cbor`].
    pub manifest_cbor: Vec<u8>,
    /// As [`FfiSyncEntry::metadata_blob`].
    pub metadata_blob: Vec<u8>,
    /// As [`FfiSyncEntry::local_chain_head`].
    pub local_chain_head: Option<Vec<u8>>,
}

/// How [`FfiWorkspace::open_synced_original`] resolved. Only `Opened` leaves anything at the
/// destination path.
// `Opened` and the other verdicts carry a whole flattened facts record or apply outcome; see
// `FfiSyncApplyOutcome` for why the payload is not boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiOpenedOriginal {
    /// Verified end to end and decrypted: the destination now holds the original's plaintext,
    /// which hashes to the signed sidecar's digest.
    Opened {
        /// The verified facts of the entry.
        facts: FfiAssetFacts,
    },
    /// The entry did not apply, exactly as
    /// [`apply_sync_entry`](FfiWorkspace::apply_sync_entry) would say: held pending or
    /// quarantined. A ciphertext file that is not the original the manifest commits to is
    /// quarantined as `Rejected.CiphertextHashMismatch`.
    Refused {
        /// The verdict, never `Applied`.
        outcome: FfiSyncApplyOutcome,
    },
    /// The entry applied, but its file key could not be recovered or its ciphertext failed
    /// authentication under it.
    Undecryptable {
        /// The verified facts of the entry.
        facts: FfiAssetFacts,
    },
    /// The ciphertext opened, but the plaintext does not hash to the signed sidecar's digest.
    DigestMismatch {
        /// The verified facts of the entry.
        facts: FfiAssetFacts,
    },
}

impl<W> From<RemoteOriginal<W>> for FfiOpenedOriginal {
    fn from(original: RemoteOriginal<W>) -> Self {
        match original {
            RemoteOriginal::Opened { facts, .. } => Self::Opened {
                facts: (*facts).into(),
            },
            RemoteOriginal::Refused(outcome) => Self::Refused {
                outcome: outcome.into(),
            },
            RemoteOriginal::Undecryptable { facts } => Self::Undecryptable {
                facts: (*facts).into(),
            },
            RemoteOriginal::DigestMismatch { facts } => Self::DigestMismatch {
                facts: (*facts).into(),
            },
        }
    }
}

impl FfiSyncHead {
    /// The entry's structural arguments, parsed: the album id and the caller's chain head.
    /// Cheap, so a verb checks them before it reads any file.
    fn parsed(&self) -> Result<(Uuid, Option<Hash32>), FfiError> {
        Ok((
            uuid_from_bytes("album_id", &self.album_id)?,
            chain_head(self.local_chain_head.as_deref())?,
        ))
    }

    /// The core head for this entry, its original named by `original_ciphertext_hash`.
    fn remote(&self, original_ciphertext_hash: Hash32) -> Result<RemoteHead<'_>, FfiError> {
        let (album_id, local_chain_head) = self.parsed()?;
        Ok(RemoteHead {
            album_id,
            manifest_cbor: &self.manifest_cbor,
            metadata_blob: &self.metadata_blob,
            original_ciphertext_hash,
            local_chain_head,
        })
    }
}

/// The verified facts one applied entry contributes, flattened for a catalog upsert.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAssetFacts {
    /// The asset id (UUID string).
    pub asset_id: String,
    /// The album id (UUID string).
    pub album_id: String,
    /// The AMK epoch the asset is sealed under.
    pub amk_version: u32,
    /// The original's ciphertext content address (lowercase hex).
    pub ciphertext_hash: String,
    /// The original's plaintext length in bytes.
    pub plaintext_size: u64,
    /// The head manifest's lifecycle action (`create`, `metadata-update`, `delete`, …).
    pub action: String,
    /// RFC3339 authoring timestamp of the head manifest.
    pub timestamp: String,
    /// The device that authored it (UUID string), resolved in the signed device directory.
    pub created_by_device: String,
    /// The 32-byte provenance head this entry establishes — persist it and pass it back as
    /// [`FfiSyncEntry::local_chain_head`] next time.
    pub provenance_head: Vec<u8>,
    /// The decrypted, signature-checked metadata.
    ///
    /// `None` exactly when the action mints no metadata blob — a `delete` tombstone or a
    /// `trash-restore`. Those are fully-verified entries that simply carry no metadata, so an
    /// app applies the lifecycle change and leaves the row's metadata as it stands.
    pub metadata: Option<FfiAssetMetadata>,
}

/// The decrypted sidecar's facts, flattened for a catalog upsert.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAssetMetadata {
    /// Sidecar schema version.
    pub sidecar_schema: u16,
    /// The original's MIME type.
    pub content_type: String,
    /// RFC3339 capture time the asset is dated by: the user's correction when one is recorded,
    /// else the as-imported value.
    pub capture_timestamp: String,
    /// Pixel width, when the sidecar records dimensions.
    pub width: Option<u32>,
    /// Pixel height, when the sidecar records dimensions.
    pub height: Option<u32>,
    /// LQIP chromahash bytes — renderable the instant metadata syncs, at zero extra request.
    pub lqip_chromahash: Option<Vec<u8>>,
    /// LQIP dominant colour, as three RGB bytes.
    pub lqip_dominant_color: Option<Vec<u8>>,
    /// The caption LWW register's current value, if any.
    pub caption: Option<String>,
    /// The user-tag OR-set's current members, sorted.
    pub tags: Vec<String>,
    /// The rating LWW register's current value, if any.
    pub rating: Option<u8>,
    /// Whether the asset is hidden from default views.
    pub hidden: bool,
}

impl From<SidecarV1> for FfiAssetMetadata {
    fn from(sidecar: SidecarV1) -> Self {
        let capture_timestamp = sidecar.effective_capture_timestamp().to_owned();
        Self {
            sidecar_schema: sidecar.sidecar_schema,
            content_type: sidecar.content_type,
            capture_timestamp,
            width: sidecar.dimensions.as_ref().map(|d| d.width),
            height: sidecar.dimensions.as_ref().map(|d| d.height),
            lqip_chromahash: sidecar.lqip.as_ref().map(|l| l.chromahash.clone()),
            lqip_dominant_color: sidecar.lqip.as_ref().map(|l| l.dominant_color.to_vec()),
            caption: sidecar.caption.get().cloned(),
            tags: sidecar.tags_user.value().into_iter().collect(),
            rating: sidecar.rating.get().copied(),
            hidden: sidecar.hidden.get().copied().unwrap_or(false),
        }
    }
}

impl From<RemoteAssetFacts> for FfiAssetFacts {
    fn from(facts: RemoteAssetFacts) -> Self {
        Self {
            asset_id: facts.asset_id.to_string(),
            album_id: facts.album_id.to_string(),
            amk_version: facts.amk_version,
            ciphertext_hash: facts.ciphertext_hash.to_hex(),
            plaintext_size: facts.plaintext_size,
            action: wire_enum(&facts.action),
            timestamp: facts.timestamp,
            created_by_device: facts.created_by_device.to_string(),
            provenance_head: facts.provenance_head.as_bytes().to_vec(),
            metadata: facts.sidecar.map(FfiAssetMetadata::from),
        }
    }
}

/// How applying one remote entry resolved — the three verdicts the client validation duties
/// permit. There is no "ignored" case: a failure is always a named quarantine or a hold.
// The `Applied` variant is a whole flattened facts record and the refusal variants are two
// strings, so the sizes are inherently lopsided. Boxing the payload is the usual fix and is not
// available here: uniffi lowers a record by value, and an indirection it cannot express would
// buy nothing on a type that crosses the FFI boundary once per feed entry.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSyncApplyOutcome {
    /// Verified end to end; upsert the facts.
    Applied {
        /// The verified facts.
        facts: FfiAssetFacts,
    },
    /// Authorization is intact but the epoch's AMK has not arrived locally yet. Hold and retry
    /// — never quarantine.
    Pending {
        /// Stable reason-code string (`AmkNotYetLocal`).
        reason: String,
    },
    /// Refused. `reason` is a stable, switchable code; `detail` is English developer text.
    Quarantined {
        /// A stable code, dotted where a sub-reason applies — e.g. `Rejected.ForgedChain`,
        /// `Binding.BlobHashMismatch`, `SidecarSignature`, `LegacyInheritedDevice` (a pre-fix
        /// continuation attributed to the wrong device of the same account; not a forgery),
        /// `Purged` (this device purged the asset; its tombstone refuses every later entry).
        reason: String,
        /// English detail, when the refusal carries one (a parse message, an album or asset id).
        detail: Option<String>,
    },
}

impl From<SyncApplyOutcome> for FfiSyncApplyOutcome {
    fn from(outcome: SyncApplyOutcome) -> Self {
        match outcome {
            SyncApplyOutcome::Applied(facts) => Self::Applied {
                facts: (*facts).into(),
            },
            SyncApplyOutcome::Pending(reason) => Self::Pending {
                reason: format!("{reason:?}"),
            },
            SyncApplyOutcome::Quarantined(reason) => {
                let (reason, detail) = match reason {
                    QuarantineReason::MalformedManifest(detail) => {
                        ("MalformedManifest".to_string(), Some(detail))
                    }
                    QuarantineReason::UnsupportedRecordVersion { found, max_known } => (
                        "UnsupportedRecordVersion".to_string(),
                        Some(format!(
                            "record version {found} is newer than this build reads ({max_known})"
                        )),
                    ),
                    QuarantineReason::UnknownAlbum(album) => {
                        ("UnknownAlbum".to_string(), Some(album.to_string()))
                    }
                    QuarantineReason::Rejected(reject) => (format!("Rejected.{reject:?}"), None),
                    QuarantineReason::LegacyInheritedDevice {
                        named_device,
                        signing_device,
                        record_hash,
                    } => (
                        "LegacyInheritedDevice".to_string(),
                        Some(format!(
                            "record {record_hash} names device {named_device} but was signed by \
                             device {signing_device}; the chain stalls at this record"
                        )),
                    ),
                    QuarantineReason::Binding(binding) => (format!("Binding.{binding:?}"), None),
                    QuarantineReason::MalformedSidecar(detail) => {
                        ("MalformedSidecar".to_string(), Some(detail))
                    }
                    QuarantineReason::SidecarSignature => ("SidecarSignature".to_string(), None),
                    QuarantineReason::Purged(asset) => {
                        ("Purged".to_string(), Some(asset.to_string()))
                    }
                };
                Self::Quarantined { reason, detail }
            }
        }
    }
}

// ─── Labels and regions ──────────────────────────────────────────────────────

/// A region's outline, in coordinates normalized to `[0, 1]` over the original's display
/// orientation. Mirrors `capsule_core::sidecar::RegionShape`, single precision included, so a
/// shape that validates on one side validates on the other.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Enum)]
pub enum FfiRegionShape {
    /// A circle centred at `(cx, cy)` (fractions of width and height) with radius `r`, a
    /// fraction of the image's shorter side.
    Circle {
        /// Centre, horizontal.
        cx: f32,
        /// Centre, vertical.
        cy: f32,
        /// Radius.
        r: f32,
    },
    /// A rectangle with its top-left corner at `(x, y)`.
    Rect {
        /// Left edge.
        x: f32,
        /// Top edge.
        y: f32,
        /// Width.
        w: f32,
        /// Height.
        h: f32,
    },
}

impl From<FfiRegionShape> for RegionShape {
    fn from(shape: FfiRegionShape) -> Self {
        match shape {
            FfiRegionShape::Circle { cx, cy, r } => Self::Circle { cx, cy, r },
            FfiRegionShape::Rect { x, y, w, h } => Self::Rect { x, y, w, h },
        }
    }
}

impl From<RegionShape> for FfiRegionShape {
    fn from(shape: RegionShape) -> Self {
        match shape {
            RegionShape::Circle { cx, cy, r } => Self::Circle { cx, cy, r },
            RegionShape::Rect { x, y, w, h } => Self::Rect { x, y, w, h },
        }
    }
}

/// One live region of an asset and its labels.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRegion {
    /// The region id (UUIDv7, minted fresh per create, never reused).
    pub region_id: String,
    /// The outline.
    pub shape: FfiRegionShape,
    /// The region's labels, sorted.
    pub labels: Vec<String>,
}

impl FfiRegion {
    fn new(region_id: Uuid, region: &Region) -> Self {
        Self {
            region_id: region_id.to_string(),
            shape: region.shape.into(),
            labels: region.labels.value().into_iter().collect(),
        }
    }
}

/// What a label attaches to beyond the asset itself, whose own labels are its user tags. The
/// label rule is the same at every level.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLabelTarget {
    /// A region of an asset.
    Region {
        /// The asset (UUID string).
        asset_id: String,
        /// The region (UUID string).
        region_id: String,
    },
    /// A stack (UUID string).
    Stack {
        /// The stack.
        stack_id: String,
    },
    /// A container album (UUID string).
    Album {
        /// The album.
        album_id: String,
    },
}

/// A label target with its ids parsed.
#[derive(Debug, Clone, Copy)]
enum LabelTarget {
    Region { asset_id: Uuid, region_id: Uuid },
    Stack(Uuid),
    Album(Uuid),
}

impl TryFrom<&FfiLabelTarget> for LabelTarget {
    type Error = FfiError;

    fn try_from(target: &FfiLabelTarget) -> Result<Self, FfiError> {
        Ok(match target {
            FfiLabelTarget::Region {
                asset_id,
                region_id,
            } => Self::Region {
                asset_id: parse_uuid("asset_id", asset_id)?,
                region_id: parse_uuid("region_id", region_id)?,
            },
            FfiLabelTarget::Stack { stack_id } => Self::Stack(parse_uuid("stack_id", stack_id)?),
            FfiLabelTarget::Album { album_id } => Self::Album(parse_uuid("album_id", album_id)?),
        })
    }
}

/// Why a label or region edit was refused ([`FfiError::Label`]). Nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiLabelRefusal {
    /// The asset, region, stack or album does not exist, or no longer does.
    NotFound,
    /// The label is empty once trimmed.
    LabelEmpty,
    /// The normalized label is this many UTF-8 bytes, past the 256-byte bound.
    LabelTooLong {
        /// The normalized label's length.
        bytes: u64,
    },
    /// A coordinate is not a finite number in `[0, 1]`.
    RegionOutOfRange,
    /// A radius, width or height is zero.
    RegionEmpty,
    /// The shape extends past an edge of the image.
    RegionPastEdge,
}

/// A label or region verb's failure: the refusals an editor names as [`FfiError::Label`],
/// everything else as the workspace failure it is.
fn label_error(error: LifecycleError) -> FfiError {
    let refusal = match &error {
        LifecycleError::NotFound(_) => FfiLabelRefusal::NotFound,
        LifecycleError::InvalidLabel(LabelError::Empty) => FfiLabelRefusal::LabelEmpty,
        LifecycleError::InvalidLabel(LabelError::TooLong(bytes)) => FfiLabelRefusal::LabelTooLong {
            bytes: u64::try_from(*bytes).unwrap_or(u64::MAX),
        },
        LifecycleError::InvalidRegion(RegionError::OutOfRange) => FfiLabelRefusal::RegionOutOfRange,
        LifecycleError::InvalidRegion(RegionError::Empty) => FfiLabelRefusal::RegionEmpty,
        LifecycleError::InvalidRegion(RegionError::PastEdge) => FfiLabelRefusal::RegionPastEdge,
        _ => return error.into(),
    };
    tracing::debug!(?refusal, %error, "ffi: label edit refused");
    FfiError::Label {
        refusal,
        detail: error.to_string(),
    }
}

/// The labels `target` carries, sorted.
fn target_labels(ws: &Workspace, target: LabelTarget) -> Result<Vec<String>, LifecycleError> {
    Ok(match target {
        LabelTarget::Region {
            asset_id,
            region_id,
        } => ws
            .regions(&asset_id)
            .into_iter()
            .find(|(id, _)| *id == region_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("region {region_id} of {asset_id}")))?
            .1
            .labels
            .value()
            .into_iter()
            .collect(),
        LabelTarget::Stack(stack_id) => ws.stack_labels(stack_id)?.into_iter().collect(),
        LabelTarget::Album(album_id) => ws.album_labels(&album_id)?.into_iter().collect(),
    })
}

/// `asset_id`'s live regions, oldest first, or [`LifecycleError::NotFound`] for an asset the
/// workspace does not hold.
fn asset_regions(ws: &Workspace, asset_id: Uuid) -> Result<Vec<FfiRegion>, LifecycleError> {
    if ws.asset(&asset_id).is_none() {
        return Err(LifecycleError::NotFound(format!("asset {asset_id}")));
    }
    // Region ids are UUIDv7 and the map is ordered by id, so id order is creation order.
    Ok(ws
        .regions(&asset_id)
        .iter()
        .map(|(id, region)| FfiRegion::new(*id, region))
        .collect())
}

// ─── Trash ───────────────────────────────────────────────────────────────────

/// One asset in the trash, as its latest signed `delete` record describes it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiTrashEntry {
    /// The trashed asset (UUID string).
    pub asset_id: String,
    /// When the `delete` was signed (RFC 3339, UTC).
    pub deleted_at: String,
    /// The signed retention floor (RFC 3339, UTC): a restore is refused at or after it. `None`
    /// for a `delete` that carries no readable window, which never expires.
    pub retention_until: Option<String>,
    /// Whether a restore is accepted now, by this device's clock. Computed by the workspace so
    /// no app re-implements the window; the server judges the same floor by its own clock.
    pub restorable: bool,
}

impl FfiTrashEntry {
    fn new(entry: &TrashEntry, now: Timestamp) -> Self {
        Self {
            asset_id: entry.asset_id.to_string(),
            deleted_at: entry.deleted_at.to_string(),
            retention_until: entry.retention_until.map(|until| until.to_string()),
            restorable: entry.is_restorable_at(now),
        }
    }
}

/// What a [`FfiWorkspace::purge_asset`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPurgeOutcome {
    /// The purged asset (UUID string).
    pub asset_id: String,
    /// The signed shorter-retention `delete` an early purge appended, as the opaque canonical
    /// CBOR provenance record [`FfiWorkspace::provenance_head`] would have returned. Send it to
    /// the album's ops surface so the server may purge too; when that delivery fails,
    /// [`FfiWorkspace::purged_record`] hands the same record back from the kept chain. `None`
    /// when the window had closed.
    pub reissue_record: Option<Vec<u8>>,
}

/// A sync that drained the feed to its head, the evidence [`FfiWorkspace::expired_trash`]
/// needs before it judges a retention deadline.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiFeedDrain {
    /// The latest server `changed_at` the drain saw (RFC 3339), or `None` for a drain that saw
    /// no entry. It floors this device's clock, so a slow clock cannot hold open a window the
    /// server has closed.
    pub latest_changed_at: Option<String>,
}

impl FfiFeedDrain {
    fn parse(&self) -> Result<FeedDrain, FfiError> {
        let latest_changed_at = self
            .latest_changed_at
            .as_deref()
            .map(str::parse::<Timestamp>)
            .transpose()
            .map_err(|e| FfiError::InvalidArgument {
                detail: format!("latest_changed_at: not an RFC 3339 timestamp ({e})"),
            })?;
        Ok(FeedDrain { latest_changed_at })
    }
}

/// Why a trash verb was refused ([`FfiError::Trash`]). Nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTrashRefusal {
    /// The workspace holds no such asset.
    NotFound,
    /// The asset is not in the trash: a restore has nothing to undo, and a purge never skips
    /// the trash.
    NotInTrash,
    /// The signed retention window has closed, so the asset can no longer be restored. It
    /// stays in the trash until it is purged.
    RetentionElapsed,
}

/// A trash verb's failure: the refusals a trash view names as [`FfiError::Trash`], everything
/// else as the workspace failure it is.
pub(super) fn trash_error(error: LifecycleError) -> FfiError {
    let (refusal, code, retention_until) = match &error {
        LifecycleError::NotFound(_) => (FfiTrashRefusal::NotFound, None, None),
        LifecycleError::NotInTrash(_) => (FfiTrashRefusal::NotInTrash, None, None),
        LifecycleError::RetentionElapsed {
            retention_until, ..
        } => (
            FfiTrashRefusal::RetentionElapsed,
            Some(capsule_i18n::error_codes::UPLOAD_RETENTION_ELAPSED.to_owned()),
            Some(retention_until.clone()),
        ),
        _ => return error.into(),
    };
    tracing::debug!(?refusal, %error, "ffi: trash verb refused");
    FfiError::Trash {
        refusal,
        code,
        retention_until,
        detail: error.to_string(),
    }
}

// ─── The workspace object ────────────────────────────────────────────────────

/// A Capsule workspace, callable from Swift/Kotlin: the durable library at a filesystem root
/// plus everything an app does with it.
///
/// `Workspace` mutates through `&mut self` while uniffi objects expose only `&self`; the
/// `Mutex` supplies the interior mutability and serializes concurrent foreign calls.
#[derive(uniffi::Object)]
pub struct FfiWorkspace {
    inner: Mutex<Workspace>,
}

impl FfiWorkspace {
    /// Run `f` under the workspace lock, surfacing a poisoned lock as a typed error rather
    /// than a panic across the boundary.
    pub(super) fn with<T>(
        &self,
        f: impl FnOnce(&mut Workspace) -> Result<T, FfiError>,
    ) -> Result<T, FfiError> {
        let mut guard = self.inner.lock().map_err(|_| FfiError::Workspace {
            detail: "workspace lock poisoned".into(),
        })?;
        f(&mut guard)
    }

    fn wrap(workspace: Workspace) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(workspace),
        })
    }

    /// The asset's upload bundle, built under the lock and returned owned, so a push drives it
    /// across the network without holding the workspace. Its original is a re-derivable
    /// source, never a buffer.
    pub(super) fn bundle(&self, asset_id: &str) -> Result<UploadBundle, FfiError> {
        let asset_id = parse_uuid("asset_id", asset_id)?;
        self.with(|ws| Ok(ws.upload_bundle(&asset_id)?))
    }

    /// Re-seal a create the server `refused` at the album's current epoch, under the lock
    /// (issue #822).
    ///
    /// A re-seal that cannot happen is reported as the push failure it is: an
    /// [`FfiError::Upload`] carrying the server's code, with a detail naming why the asset
    /// cannot be re-sealed. When this device holds no newer epoch than the asset's, the detail
    /// says only that there is no newer epoch to re-seal at, because the server refuses a revoked
    /// device or withdrawn write access the same way and no key need be missing.
    pub(super) fn reseal(&self, asset_id: &Uuid, refused: &UploadError) -> Result<(), FfiError> {
        self.with(|ws| match ws.reseal_create(asset_id) {
            Ok(outcome) => {
                tracing::info!(
                    asset_id = %asset_id,
                    from_epoch = outcome.from_epoch,
                    to_epoch = outcome.to_epoch,
                    derivatives = outcome.derivatives,
                    "ffi: refused create re-sealed at the album's current epoch"
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(asset_id = %asset_id, %error, "ffi: the refused create cannot be re-sealed");
                let code = match refused {
                    UploadError::Rejected { code, .. } => code.clone(),
                    _ => None,
                };
                Err(FfiError::Upload {
                    code,
                    detail: format!("the server refused this create ({refused}); {error}"),
                })
            }
        })
    }
}

/// Parse a UUID from foreign code into a typed error rather than panicking at the boundary.
pub(super) fn parse_uuid(field: &str, value: &str) -> Result<Uuid, FfiError> {
    Uuid::parse_str(value).map_err(|e| FfiError::InvalidArgument {
        detail: format!("{field}: {value:?} is not a UUID ({e})"),
    })
}

/// Parse the feed's 16-byte UUID form.
fn uuid_from_bytes(field: &str, bytes: &[u8]) -> Result<Uuid, FfiError> {
    Uuid::from_slice(bytes).map_err(|e| FfiError::InvalidArgument {
        detail: format!("{field}: expected 16 UUID bytes ({e})"),
    })
}

/// Parse an optional 32-byte provenance head.
fn chain_head(head: Option<&[u8]>) -> Result<Option<Hash32>, FfiError> {
    head.map(|head| {
        <[u8; 32]>::try_from(head)
            .map(Hash32::from_bytes)
            .map_err(|_| FfiError::InvalidArgument {
                detail: "local_chain_head must be exactly 32 bytes".into(),
            })
    })
    .transpose()
}

/// A local file failure, as the workspace error it is.
fn local_io(action: &str, path: &Path, error: &std::io::Error) -> FfiError {
    FfiError::Workspace {
        detail: format!("{action} {}: {error}", path.display()),
    }
}

/// A fresh hidden file beside `destination`, on the same filesystem so that renaming it into
/// place is atomic, and named uniquely so that two opens never share one.
fn staging_path(destination: &Path) -> Result<PathBuf, FfiError> {
    let name = destination
        .file_name()
        .ok_or_else(|| FfiError::InvalidArgument {
            detail: format!("destination_path: {} names no file", destination.display()),
        })?;
    let mut staged = std::ffi::OsString::from(".");
    staged.push(name);
    staged.push(format!(".{}.part", Uuid::now_v7()));
    Ok(destination.with_file_name(staged))
}

/// Move a fully written staging file into place as `target`, so that `target` is either
/// untouched or the complete original: flushed and synced to disk, then renamed.
fn commit_staged(
    sink: std::io::BufWriter<std::fs::File>,
    staged: &Path,
    target: &Path,
) -> std::io::Result<()> {
    let file = sink
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(staged, target)?;
    // Persist the rename itself. Best effort: not every platform opens a directory for sync.
    if let Some(parent) = target.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Remove a staging file that will not be renamed into place.
fn discard_staged(staged: &Path) {
    match std::fs::remove_file(staged) {
        Ok(()) => {
            tracing::debug!(staged = %staged.display(), "ffi: unverified staging file removed");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(staged = %staged.display(), %error, "ffi: staging file could not be removed");
        }
    }
}

/// The ladder tier's stable lowercase name (`index` / `preview` / `original`) — the same three
/// rungs the transfer download tier ladder names, so an app's progress UI can key on one vocabulary
/// in both directions.
pub(super) fn tier_name(tier: capsule_library::import::UploadTier) -> String {
    use capsule_library::import::UploadTier;
    match tier {
        UploadTier::Index => "index",
        UploadTier::Preview => "preview",
        UploadTier::Original => "original",
    }
    .to_string()
}

/// Serialize a wire enum (`Action`, …) to its bare protocol string, the same projection the
/// upload envelope uses.
fn wire_enum<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[uniffi::export]
impl FfiWorkspace {
    /// **Enroll**: create a brand-new library at `root`, guarded by `passphrase` at the
    /// Argon2id cost for `tier`. Mints the account, the device key, and the signed device
    /// directory, and seeds the durable album keystore.
    ///
    /// `client` is the app's own build identity: every manifest this workspace authors then
    /// reports `client_id/semver+commit` rather than the bare `capsule-core` default.
    #[uniffi::constructor]
    pub fn create(
        root: String,
        passphrase: Vec<u8>,
        tier: FfiDeviceTier,
        client: FfiClientBuild,
    ) -> Result<Arc<Self>, FfiError> {
        let workspace = Workspace::create(&PathBuf::from(root), &passphrase, tier.into())?
            .with_client_id(&client.client_id, &client.semver);
        tracing::info!(user_id = %workspace.user_id(), "workspace created");
        Ok(Self::wrap(workspace))
    }

    /// **Enroll with a hardware-bound Ed25519 device key**: as [`create`](Self::create), but
    /// the classical half of the device signing key is produced by `hardware` under
    /// `key_alias` over **Ed25519** (a 32-byte public key, 64-byte signatures). The
    /// post-quantum ML-DSA-65 half is the software-sealed `ml_seed` (32 bytes).
    ///
    /// No shipping secure element holds Ed25519 keys, so this is the software/TPM reference
    /// composition; phones use
    /// [`create_with_p256_hardware_signer`](Self::create_with_p256_hardware_signer). It is the
    /// same core call (`HardwareBackedSigner::enroll`) reached through this namespace's
    /// [`FfiHardwareSigner`] seam.
    #[uniffi::constructor]
    pub fn create_with_hardware_signer(
        root: String,
        passphrase: Vec<u8>,
        tier: FfiDeviceTier,
        hardware: Arc<dyn FfiHardwareSigner>,
        key_alias: String,
        ml_seed: Vec<u8>,
        client: FfiClientBuild,
    ) -> Result<Arc<Self>, FfiError> {
        let seed = parse_ml_seed(&ml_seed)?;
        let signer = HardwareBackedSigner::enroll(
            Arc::new(ForeignHardwareSigner(hardware)),
            key_alias,
            &seed,
        )
        .map_err(|e| FfiError::Workspace {
            detail: format!("hardware Ed25519 key enrollment failed: {e}"),
        })?;
        let workspace = Workspace::create_with_hardware_signer(
            &PathBuf::from(root),
            &passphrase,
            DeviceTier::from(tier).params(),
            Box::new(signer) as Box<dyn Signer>,
        )?
        .with_client_id(&client.client_id, &client.semver);
        tracing::info!(
            user_id = %workspace.user_id(),
            "workspace created with a hardware-bound Ed25519 device key"
        );
        Ok(Self::wrap(workspace))
    }

    /// **Enroll with a hardware-bound device key**: as [`create`](Self::create), but the
    /// classical half of the device signing key is produced by `hardware` (a native Secure
    /// Enclave / StrongBox / TPM element) under `key_alias`, over **ECDSA-P256** — the
    /// composition shipping secure elements actually provide, none of which do Ed25519. The
    /// post-quantum ML-DSA-65 half is the software-sealed `ml_seed` (32 bytes).
    ///
    /// The published device key carries the P-256 tag and `verify_asset` dispatches on it. The
    /// library records the binding (the alias, `ml_seed` sealed under the account master key,
    /// and the published key), so every later launch reopens it with
    /// [`open_with_p256_hardware_signer`](Self::open_with_p256_hardware_signer) on the same
    /// key; plain [`open`](Self::open) refuses such a library. An element that refuses
    /// enrollment fails this call before anything is written under `root`.
    #[uniffi::constructor]
    pub fn create_with_p256_hardware_signer(
        root: String,
        passphrase: Vec<u8>,
        tier: FfiDeviceTier,
        hardware: Arc<dyn FfiHardwareSigner>,
        key_alias: String,
        ml_seed: Vec<u8>,
        client: FfiClientBuild,
    ) -> Result<Arc<Self>, FfiError> {
        let seed = parse_ml_seed(&ml_seed)?;
        let workspace = Workspace::create_with_p256_hardware_signer(
            &PathBuf::from(root),
            &passphrase,
            DeviceTier::from(tier).params(),
            HardwareSignerBinding {
                element: Arc::new(ForeignHardwareSigner(hardware)),
                key_alias,
            },
            &seed,
        )?
        .with_client_id(&client.client_id, &client.semver);
        tracing::info!(
            user_id = %workspace.user_id(),
            "workspace created with a hardware-bound P-256 device key"
        );
        Ok(Self::wrap(workspace))
    }

    /// **Open** the existing library at `root` as [`open`](Self::open) does, re-attaching
    /// `hardware`, the element that holds this device's P-256 signing key. Required for a
    /// library made by
    /// [`create_with_p256_hardware_signer`](Self::create_with_p256_hardware_signer): the element
    /// is asked for the key under the alias the library recorded, never to make a new one, and
    /// the rebuilt key must be the one this device published. An element that lost the key or
    /// is unavailable fails the open with [`FfiError::Workspace`] rather than letting the
    /// library sign with another key.
    ///
    /// The library's record decides, not the caller: a library created with the software key
    /// opens on it and `hardware` is not used. `key_alias` is used only when `root` holds a
    /// laid-out library with no account yet, whose fresh account is then enrolled under it.
    #[uniffi::constructor]
    pub fn open_with_p256_hardware_signer(
        root: String,
        passphrase: Vec<u8>,
        tier: FfiDeviceTier,
        hardware: Arc<dyn FfiHardwareSigner>,
        key_alias: String,
        client: FfiClientBuild,
    ) -> Result<Arc<Self>, FfiError> {
        let workspace = Workspace::open_with_hardware_signer(
            &PathBuf::from(root),
            &passphrase,
            DeviceTier::from(tier).params(),
            HardwareSignerBinding {
                element: Arc::new(ForeignHardwareSigner(hardware)),
                key_alias,
            },
        )?
        .with_client_id(&client.client_id, &client.semver);
        tracing::info!(
            user_id = %workspace.user_id(),
            albums = workspace.albums().len(),
            "workspace opened with a hardware signing element attached"
        );
        Ok(Self::wrap(workspace))
    }

    /// **Open** the existing library at `root` under `passphrase`, restoring its album keys and
    /// authorities, every managed asset, and the `add_id` counter from disk. This is
    /// what makes an app relaunch resume rather than re-enroll.
    #[uniffi::constructor]
    pub fn open(
        root: String,
        passphrase: Vec<u8>,
        tier: FfiDeviceTier,
        client: FfiClientBuild,
    ) -> Result<Arc<Self>, FfiError> {
        let workspace = Workspace::open(
            &PathBuf::from(root),
            &passphrase,
            DeviceTier::from(tier).params(),
        )?
        .with_client_id(&client.client_id, &client.semver);
        tracing::info!(
            user_id = %workspace.user_id(),
            albums = workspace.albums().len(),
            "workspace opened"
        );
        Ok(Self::wrap(workspace))
    }

    /// The account owner's user id (UUID string).
    pub fn user_id(&self) -> Result<String, FfiError> {
        self.with(|ws| Ok(ws.user_id().to_string()))
    }

    /// This device's id (UUID string) — the `created_by_device` of every manifest it authors,
    /// and the id to report as the session's `device_id`.
    pub fn device_id(&self) -> Result<String, FfiError> {
        self.with(|ws| Ok(ws.device_id().to_string()))
    }

    /// The default album's id, derived deterministically from the account master key — so every
    /// device of this account computes the same value with nothing to synchronize.
    pub fn default_album_id(&self) -> Result<String, FfiError> {
        self.with(|ws| Ok(ws.default_album_id().to_string()))
    }

    /// Create an album (minting AMK_v1, its write-tier and admin keys, and an attested
    /// authority, then persisting the keystore). Returns the album id.
    pub fn create_album(&self, name: String) -> Result<String, FfiError> {
        self.with(|ws| Ok(ws.create_album(&name)?.to_string()))
    }

    /// Resolve `album_id` to the album this workspace already holds, or create it under `name`.
    /// This is the verb first-run wiring wants for the default album: it is idempotent across
    /// relaunches, where [`create_album`](Self::create_album) would refuse.
    pub fn ensure_album(&self, album_id: String, name: String) -> Result<String, FfiError> {
        let album_id = parse_uuid("album_id", &album_id)?;
        self.with(|ws| Ok(ws.ensure_album(album_id, &name)?.to_string()))
    }

    /// Every album this workspace holds key material for.
    pub fn albums(&self) -> Result<Vec<FfiAlbum>, FfiError> {
        self.with(|ws| {
            Ok(ws
                .albums()
                .into_iter()
                .map(|(album_id, name)| FfiAlbum {
                    album_id: album_id.to_string(),
                    name,
                })
                .collect())
        })
    }

    /// **Seal + import**: take bytes the app already holds (a PhotoKit / MediaStore export),
    /// derive the file key, STREAM-encrypt, author and sign the create manifest, seal the
    /// signed sidecar into its metadata blob, append the provenance chain, and self-verify
    /// through the `verify_asset` chokepoint. Returns the asset id.
    ///
    /// `file_name` is consulted for its extension and nothing else. One call, one core call:
    /// the whole sealing order is `capsule-core`'s.
    pub fn seal_asset(
        &self,
        album_id: String,
        file_name: String,
        bytes: Vec<u8>,
    ) -> Result<String, FfiError> {
        let album_id = parse_uuid("album_id", &album_id)?;
        self.with(|ws| Ok(ws.import_bytes(album_id, &file_name, &bytes)?.to_string()))
    }

    /// Everything needed to push one sealed asset, in ladder order: for each blob, the
    /// `POST /upload` body (manifest envelope projected per the server's invariant-15 rule)
    /// and the bytes. Feed each straight to
    /// [`FfiSession::upload`](crate::ffi::FfiSession::upload).
    ///
    /// The index tier (`T0`) is **two** blobs — the provenance record then the sealed metadata
    /// blob — because the server publishes an asset to other devices only once it holds both
    /// roles. Push all of them: an app that stopped after the metadata blob would upload an
    /// asset no device, its own included, could ever see.
    ///
    /// The original's ciphertext is re-derived from the manifest's recorded nonce prefix and
    /// gated on the manifest's own content address, so what reaches the network is what the
    /// signed manifest vouches for.
    ///
    /// Every blob comes back whole, the original included. To push without holding the
    /// original in memory, use [`FfiSession::push_asset`](crate::ffi::FfiSession::push_asset).
    pub fn upload_blobs(&self, asset_id: String) -> Result<Vec<FfiUploadBlob>, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            let bundle = ws.upload_bundle(&asset_id)?;
            crate::push::bundle_blobs(&bundle)
                .into_iter()
                .map(|(blob, hash)| {
                    // The FFI hands each blob across as one buffer, so the original is
                    // materialised here; the SDK's own push streams it instead.
                    let bytes = blob.bytes.to_vec().map_err(|e| FfiError::Workspace {
                        detail: format!("reading the original of {asset_id}: {e}"),
                    })?;
                    Ok(FfiUploadBlob {
                        tier: tier_name(blob.tier),
                        request: crate::push::create_request(&bundle, &blob, &hash).into(),
                        bytes,
                        hash,
                    })
                })
                .collect()
        })
    }

    /// Run the `verify_asset` chokepoint over a managed asset, regenerating its ciphertext
    /// deterministically. This is the same single chokepoint sync-apply routes through — it is
    /// exposed here so an app can re-check its own library (an integrity sweep), never as a
    /// second verification path.
    pub fn verify_asset(&self, asset_id: String) -> Result<FfiVerifyOutcome, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| Ok(ws.verify(&asset_id)?.into()))
    }

    /// The plaintext bytes of a managed asset (what a gallery renders for a locally-held
    /// original).
    pub fn read_plaintext(&self, asset_id: String) -> Result<Vec<u8>, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| Ok(ws.read_plaintext(&asset_id)?))
    }

    /// Every managed asset id (UUID strings).
    pub fn asset_ids(&self) -> Result<Vec<String>, FfiError> {
        self.with(|ws| Ok(ws.asset_ids().iter().map(Uuid::to_string).collect()))
    }

    /// The asset's **head provenance record** as opaque canonical CBOR — the exact bytes the
    /// `provenance` blob carries, the feed serves back as `manifest_cbor`, and a receiving
    /// device runs through [`apply_sync_entry`](Self::apply_sync_entry).
    ///
    /// The record wraps the head signed manifest with its chain position. That wrapper is not
    /// optional: the server's chain head is the SHA-256 of these bytes while a client's next
    /// `prior_provenance_hash` is the record's own hash, so the bare manifest is the one
    /// encoding under which no lifecycle op can ever chain. Canonical CBOR is deterministic, so
    /// the manifest inside is byte-identical to its own signed bytes — nothing is re-authored
    /// and the two signatures still verify over exactly what they covered.
    ///
    /// Use [`signed_manifest`](Self::signed_manifest) when the *manifest* is what is wanted.
    pub fn provenance_head(&self, asset_id: String) -> Result<Vec<u8>, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        // Read off the chain head directly: the upload bundle carries the create record, which
        // is the head only until the first delete, restore or edit.
        self.with(|ws| {
            let asset = ws.asset(&asset_id).ok_or_else(|| FfiError::Workspace {
                detail: format!("asset {asset_id} is not managed by this workspace"),
            })?;
            let head = asset
                .chain
                .records()
                .last()
                .ok_or_else(|| FfiError::Workspace {
                    detail: format!("asset {asset_id} has an empty provenance chain"),
                })?;
            capsule_core::cbor::to_canonical_vec(head).map_err(|e| FfiError::Workspace {
                detail: format!("reading the provenance head of {asset_id}: {e}"),
            })
        })
    }

    /// The asset's **head signed manifest** as opaque canonical CBOR.
    ///
    /// This is a serialization of an already-signed structure, not a re-authoring: the two
    /// signatures the manifest carries are covered by these bytes, which is why they travel
    /// verbatim and are never re-modeled on any wire.
    ///
    /// **Not what the sync feed carries** — that is
    /// [`provenance_head`](Self::provenance_head), the record that wraps this manifest with its
    /// chain position. Handing these bytes to
    /// [`apply_sync_entry`](Self::apply_sync_entry) is refused as malformed.
    pub fn signed_manifest(&self, asset_id: String) -> Result<Vec<u8>, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            let asset = ws.asset(&asset_id).ok_or_else(|| FfiError::Workspace {
                detail: format!("asset {asset_id} is not managed by this workspace"),
            })?;
            let head = asset
                .chain
                .records()
                .last()
                .ok_or_else(|| FfiError::Workspace {
                    detail: format!("asset {asset_id} has an empty provenance chain"),
                })?;
            capsule_core::cbor::to_canonical_vec(&head.manifest).map_err(|e| FfiError::Workspace {
                detail: format!("encoding the signed manifest failed: {e}"),
            })
        })
    }

    /// **Sync-apply**: verify one remote feed entry through the `verify_asset` chokepoint,
    /// bind and decrypt its metadata blob, and return either the facts to upsert or a named
    /// quarantine/hold verdict.
    ///
    /// Never silently drops and never silently accepts: a refused entry is a
    /// [`FfiSyncApplyOutcome::Quarantined`] the app must surface, not an error that aborts the
    /// page. Errors are reserved for a failure of this workspace itself.
    pub fn apply_sync_entry(&self, entry: FfiSyncEntry) -> Result<FfiSyncApplyOutcome, FfiError> {
        let album_id = uuid_from_bytes("album_id", &entry.album_id)?;
        let local_chain_head = chain_head(entry.local_chain_head.as_deref())?;
        self.with(|ws| {
            let outcome = ws.apply_remote_entry(RemoteEntry {
                album_id,
                manifest_cbor: &entry.manifest_cbor,
                metadata_blob: &entry.metadata_blob,
                original_ciphertext: &entry.original_ciphertext,
                local_chain_head,
            })?;
            Ok(outcome.into())
        })
    }

    /// [`apply_sync_entry`](Self::apply_sync_entry) for an entry whose original ciphertext is
    /// the file at `ciphertext_path` — the one
    /// [`FfiSession::fetch_blob_to_path`](crate::ffi::FfiSession::fetch_blob_to_path) wrote —
    /// read in memory bounded by one STREAM chunk whatever the original's size.
    ///
    /// The same chokepoint and verdicts: the file is hashed, and that digest is what the
    /// content-integrity step checks against the manifest's content address, so a file that is
    /// not the committed ciphertext is quarantined as `Rejected.CiphertextHashMismatch`, as
    /// `apply_sync_entry` quarantines those bytes. Nothing is written anywhere.
    ///
    /// A malformed `album_id` or `local_chain_head` is refused before the file is opened, so a
    /// bad argument never costs a read of the original. The file is hashed before the workspace
    /// is locked, so other verbs are not held up while a large original is read.
    pub fn apply_sync_entry_from_file(
        &self,
        entry: FfiSyncHead,
        ciphertext_path: String,
    ) -> Result<FfiSyncApplyOutcome, FfiError> {
        let (album_id, local_chain_head) = entry.parsed()?;
        let path = PathBuf::from(ciphertext_path);
        let file = std::fs::File::open(&path)
            .map_err(|e| local_io("opening the ciphertext file", &path, &e))?;
        let (digest, bytes) = hash_ciphertext_from(file)?;
        tracing::debug!(path = %path.display(), bytes, "ffi: synced original hashed from disk");
        let head = RemoteHead {
            album_id,
            manifest_cbor: &entry.manifest_cbor,
            metadata_blob: &entry.metadata_blob,
            original_ciphertext_hash: digest,
            local_chain_head,
        };
        self.with(|ws| Ok(ws.apply_remote_head(head)?.into()))
    }

    /// **Open a synced original** from the ciphertext file at `ciphertext_path` to its plaintext
    /// at `destination_path`, in memory bounded by a few STREAM chunks whatever its size.
    ///
    /// The entry goes through the chokepoint as
    /// [`apply_sync_entry_from_file`](Self::apply_sync_entry_from_file) would take it, then the
    /// file is hashed while it is decrypted: it opens only if it is the ciphertext the manifest
    /// commits to, every chunk authenticates, and the plaintext hashes to the signed sidecar's
    /// digest. The plaintext is written to a hidden staging file beside the destination and
    /// renamed into place only once all of that holds, so `destination_path` is untouched by
    /// any other verdict and never holds a partial or unverified original. An existing file there
    /// is replaced only by a verified one.
    ///
    /// The workspace stays locked while the original decrypts.
    ///
    /// # Errors
    ///
    /// [`FfiError::Workspace`] when a file cannot be read or written, or the workspace itself
    /// fails; [`FfiError::InvalidArgument`] for a malformed entry or a destination with no file
    /// name. A refused entry is a verdict, never an error.
    pub fn open_synced_original(
        &self,
        entry: FfiSyncHead,
        ciphertext_path: String,
        destination_path: String,
    ) -> Result<FfiOpenedOriginal, FfiError> {
        let source = PathBuf::from(ciphertext_path);
        let destination = PathBuf::from(destination_path);
        // The claim the chokepoint checks first is the manifest's own content address, because
        // nothing has read the file yet. It is a claim only: `open_remote_original_from` hashes
        // every byte it decrypts and refuses the original unless that hash is this address. A
        // record that does not decode is quarantined by the apply before the claim is looked at.
        let committed = ProvenanceRecord::decode(&entry.manifest_cbor)
            .map_or(Hash32::from_bytes([0; 32]), |record| {
                record.manifest.core.ciphertext_hash
            });
        let head = entry.remote(committed)?;
        let ciphertext = std::fs::File::open(&source)
            .map_err(|e| local_io("opening the ciphertext file", &source, &e))?;
        let staged = staging_path(&destination)?;
        let sink = std::fs::File::create_new(&staged)
            .map(std::io::BufWriter::new)
            .map_err(|e| local_io("creating the staging file", &staged, &e))?;
        let opened = self.with(|ws| Ok(ws.open_remote_original_from(head, ciphertext, sink)?));
        let (facts, sink) = match opened {
            Ok(RemoteOriginal::Opened { facts, plaintext }) => (facts, plaintext),
            Ok(refused) => {
                discard_staged(&staged);
                tracing::warn!(source = %source.display(), "ffi: synced original not opened; nothing placed");
                return Ok(refused.into());
            }
            Err(error) => {
                discard_staged(&staged);
                return Err(error);
            }
        };
        if let Err(e) = commit_staged(sink, &staged, &destination) {
            discard_staged(&staged);
            return Err(local_io(
                "moving the original into place at",
                &destination,
                &e,
            ));
        }
        tracing::info!(
            asset_id = %facts.asset_id,
            destination = %destination.display(),
            "ffi: synced original verified and opened to disk"
        );
        Ok(FfiOpenedOriginal::Opened {
            facts: (*facts).into(),
        })
    }

    /// Mint the **master-key escrow blob** under `recovery_secret`, as opaque canonical CBOR —
    /// the bytes [`FfiSession::escrow_put`](crate::ffi::FfiSession::escrow_put) stores.
    ///
    /// The master key never crosses this boundary: it is wrapped inside `capsule-core` and only
    /// the wrapped blob comes out. `recovery_secret` is the ≥128-bit secret shown to the user
    /// exactly once at first-device enrollment.
    pub fn escrow_blob(
        &self,
        recovery_secret: Vec<u8>,
        tier: FfiDeviceTier,
    ) -> Result<Vec<u8>, FfiError> {
        self.with(|ws| {
            let blob = ws.escrow_master_key(&recovery_secret, tier.into())?;
            capsule_core::cbor::to_canonical_vec(&blob).map_err(|e| FfiError::Workspace {
                detail: format!("encoding the escrow blob failed: {e}"),
            })
        })
    }

    /// Local, network-free check that `recovery_secret` still opens `blob` to *this* device's
    /// master key — the recovery verification cadence's predicate. `blob` is the canonical CBOR
    /// [`FfiSession::escrow_get`](crate::ffi::FfiSession::escrow_get) returns.
    ///
    /// A `false` is not necessarily a wrong secret: the cached blob may be stale after a
    /// rotation on another device, so refresh it and re-check once before telling the user
    /// anything.
    pub fn verify_escrow_blob(
        &self,
        blob: Vec<u8>,
        recovery_secret: Vec<u8>,
    ) -> Result<bool, FfiError> {
        let blob =
            capsule_core::cbor::from_slice(&blob).map_err(|e| FfiError::InvalidArgument {
                detail: format!("escrow blob is not a canonical WrappedSecret: {e}"),
            })?;
        self.with(|ws| {
            Ok(ws.verify_escrow(&blob, &recovery_secret)
                == capsule_core::backup::VerifyOutcome::Verified)
        })
    }

    /// This workspace's **signed device directory** as opaque canonical CBOR — the bytes
    /// [`FfiSession::publish_device_directory`](crate::ffi::FfiSession::publish_device_directory)
    /// sends. Until this is published, every asset this device signs is an unknown device to
    /// every other device, so first-device enrollment publishes before it uploads.
    pub fn signed_device_directory(&self) -> Result<Vec<u8>, FfiError> {
        self.with(|ws| {
            capsule_core::cbor::to_canonical_vec(ws.device_directory()).map_err(|e| {
                FfiError::Workspace {
                    detail: format!("encoding the device directory failed: {e}"),
                }
            })
        })
    }

    /// This account's user identity **public** key — the pin a fetched device directory is
    /// verified against.
    pub fn user_ik_public(&self) -> Result<Vec<u8>, FfiError> {
        self.with(|ws| Ok(ws.user_ik_public().to_bytes()))
    }

    // ── Labels and regions ───────────────────────────────────────────────────
    //
    // Every edit is metadata-only: a region edit is a `metadata-update` of the asset's signed
    // sidecar, a stack or album label edit rewrites that record. Labels go in as typed; the
    // workspace normalizes them (NFC, trimmed, 1–256 bytes) and refuses one the rule refuses
    // before anything is written. Every write answers with what its target holds afterwards.

    /// The labels `target` carries, sorted. An unknown target is [`FfiError::Label`] with
    /// [`FfiLabelRefusal::NotFound`].
    pub fn labels(&self, target: FfiLabelTarget) -> Result<Vec<String>, FfiError> {
        let target = LabelTarget::try_from(&target)?;
        self.with(|ws| target_labels(ws, target).map_err(label_error))
    }

    /// Add `label` to `target` and return its labels afterwards.
    pub fn add_label(
        &self,
        target: FfiLabelTarget,
        label: String,
    ) -> Result<Vec<String>, FfiError> {
        let target = LabelTarget::try_from(&target)?;
        self.with(|ws| {
            match target {
                LabelTarget::Region {
                    asset_id,
                    region_id,
                } => ws.add_region_label(&asset_id, &region_id, &label),
                LabelTarget::Stack(stack_id) => ws.add_stack_label(stack_id, &label),
                LabelTarget::Album(album_id) => ws.add_album_label(album_id, &label),
            }
            .map_err(label_error)?;
            tracing::info!(?target, "ffi: label added");
            target_labels(ws, target).map_err(label_error)
        })
    }

    /// Remove `label` from `target` (every add of it this device has observed) and return its
    /// labels afterwards. A label the target does not carry writes nothing and answers the
    /// unchanged list.
    pub fn remove_label(
        &self,
        target: FfiLabelTarget,
        label: String,
    ) -> Result<Vec<String>, FfiError> {
        let target = LabelTarget::try_from(&target)?;
        self.with(|ws| {
            let removed = match target {
                LabelTarget::Region {
                    asset_id,
                    region_id,
                } => ws.remove_region_label(&asset_id, &region_id, &label),
                LabelTarget::Stack(stack_id) => ws.remove_stack_label(stack_id, &label),
                LabelTarget::Album(album_id) => ws.remove_album_label(album_id, &label),
            }
            .map_err(label_error)?;
            tracing::info!(?target, removed, "ffi: label removed");
            target_labels(ws, target).map_err(label_error)
        })
    }

    /// The asset's live regions, oldest first.
    pub fn regions(&self, asset_id: String) -> Result<Vec<FfiRegion>, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| asset_regions(ws, asset_id).map_err(label_error))
    }

    /// Mark a region of `asset_id` with its first `labels`, and return it.
    ///
    /// Every label is checked before the region is written, so a refused label leaves no
    /// unlabelled region behind; then the shape, then the asset. A refusal writes nothing.
    pub fn add_region(
        &self,
        asset_id: String,
        shape: FfiRegionShape,
        labels: Vec<String>,
    ) -> Result<FfiRegion, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        for label in &labels {
            normalize_label(label)
                .map_err(LifecycleError::from)
                .map_err(label_error)?;
        }
        self.with(|ws| {
            let region_id = ws
                .add_region(&asset_id, shape.into())
                .map_err(label_error)?;
            for label in &labels {
                ws.add_region_label(&asset_id, &region_id, label)
                    .map_err(label_error)?;
            }
            tracing::info!(%asset_id, %region_id, labels = labels.len(), "ffi: region added");
            ws.regions(&asset_id)
                .iter()
                .find(|(id, _)| *id == region_id)
                .map(|(id, region)| FfiRegion::new(*id, region))
                .ok_or_else(|| FfiError::Workspace {
                    detail: format!("region {region_id} was written and is not live"),
                })
        })
    }

    /// Delete a region. **Final**: a concurrent or later edit of it on another device does not
    /// bring it back once the two merge, and its id is never live again.
    pub fn delete_region(&self, asset_id: String, region_id: String) -> Result<(), FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        let region_id = parse_uuid("region_id", &region_id)?;
        self.with(|ws| {
            ws.delete_region(&asset_id, &region_id)
                .map_err(label_error)?;
            tracing::info!(%asset_id, %region_id, "ffi: region deleted");
            Ok(())
        })
    }

    // ── Trash ────────────────────────────────────────────────────────────────
    //
    // The whole deletion state machine is the workspace's: every transition is a signed
    // provenance record, so it syncs and survives an index rebuild, and every judgement of the
    // retention window reads the `retention_until` signed into the `delete` record. An app
    // calls these and keeps no retention logic of its own. A refusal is [`FfiError::Trash`].

    /// Move an asset to the trash: sign a `delete` whose retention window is `retain_days` days
    /// from now, and return its trash entry. An asset already in the trash keeps the window it
    /// was deleted with and signs nothing, so a retried delete never extends it.
    pub fn delete_asset(
        &self,
        asset_id: String,
        retain_days: u32,
    ) -> Result<FfiTrashEntry, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            if let Some(entry) = ws.trash_entry(&asset_id) {
                tracing::debug!(%asset_id, "ffi: delete of an asset already in the trash; nothing signed");
                return Ok(FfiTrashEntry::new(&entry, Timestamp::now()));
            }
            ws.soft_delete(&asset_id, i64::from(retain_days))
                .map_err(trash_error)?;
            tracing::info!(%asset_id, retain_days, "ffi: asset moved to the trash");
            let entry = ws.trash_entry(&asset_id).ok_or_else(|| FfiError::Workspace {
                detail: format!("asset {asset_id} was deleted and is not in the trash"),
            })?;
            Ok(FfiTrashEntry::new(&entry, Timestamp::now()))
        })
    }

    /// Restore an asset from the trash: sign a `trash-restore`. Refused with
    /// [`FfiTrashRefusal::RetentionElapsed`] (and the `error.upload.retention_elapsed` code) at
    /// or after the signed `retention_until`, and with [`FfiTrashRefusal::NotInTrash`] for an
    /// asset that is not in the trash.
    pub fn restore_asset(&self, asset_id: String) -> Result<(), FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            ws.restore(&asset_id).map_err(trash_error)?;
            tracing::info!(%asset_id, "ffi: asset restored from the trash");
            Ok(())
        })
    }

    /// Permanently delete a trashed asset: its original, every derivative and local cache
    /// entry, its sidecar and its index row. The provenance chain is kept as the record of the
    /// deletion. Before `retention_until` a signed shorter-retention `delete` is appended first
    /// and handed back in [`FfiPurgeOutcome::reissue_record`] to send to the server.
    pub fn purge_asset(&self, asset_id: String) -> Result<FfiPurgeOutcome, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            let outcome = ws.purge(&asset_id).map_err(trash_error)?;
            let reissue_record = outcome
                .reissue
                .as_ref()
                .map(capsule_core::cbor::to_canonical_vec)
                .transpose()
                .map_err(|e| FfiError::Workspace {
                    detail: format!("encoding the re-issued delete of {asset_id}: {e}"),
                })?;
            tracing::info!(%asset_id, removed_files = outcome.removed_files, reissued = reissue_record.is_some(), "ffi: asset purged");
            Ok(FfiPurgeOutcome {
                asset_id: asset_id.to_string(),
                reissue_record,
            })
        })
    }

    /// Every asset in the trash with its signed retention window, most recently deleted first.
    ///
    /// This is the retention data, not the Recently Deleted *view*: a surface that shows the
    /// trash still opens it behind the SR1 fresh-local-auth gate, as `Catalog::query_trash`
    /// does.
    pub fn trash(&self) -> Result<Vec<FfiTrashEntry>, FfiError> {
        let now = Timestamp::now();
        self.with(|ws| {
            Ok(ws
                .trash()
                .iter()
                .map(|entry| FfiTrashEntry::new(entry, now))
                .collect())
        })
    }

    /// The trash entries whose signed retention window has closed: the retention sweep's
    /// input, each one ready for [`purge_asset`](Self::purge_asset).
    ///
    /// A deadline is judged only after a sync drained the feed to its head, so a restore on a
    /// later page lands first: pass that drain as `drain`. The deadline is judged by this
    /// device's clock floored by the drain's latest server `changed_at`. With no drain (`None`:
    /// the sync failed, or this device has no server) nothing has expired, and the trash keeps
    /// every asset until the user empties it.
    ///
    /// # Errors
    ///
    /// [`FfiError::InvalidArgument`] for a `latest_changed_at` that is not RFC 3339.
    pub fn expired_trash(
        &self,
        drain: Option<FfiFeedDrain>,
    ) -> Result<Vec<FfiTrashEntry>, FfiError> {
        let drain = drain.as_ref().map(FfiFeedDrain::parse).transpose()?;
        let clock = Timestamp::now();
        let judged_at = drain.map_or(clock, |drain| drain.judged_at(clock));
        self.with(|ws| {
            Ok(ws
                .expired_trash_after_drain(clock, drain)
                .iter()
                .map(|entry| FfiTrashEntry::new(entry, judged_at))
                .collect())
        })
    }

    /// The signed `delete` that ended a purged asset, the head of its kept provenance chain, as
    /// the opaque canonical CBOR record [`FfiPurgeOutcome::reissue_record`] carries. After an
    /// early purge it is that same re-issue, so a delivery to the album's ops surface that
    /// failed can be resent from here, also after a reopen. `None` for an asset this workspace
    /// has not purged.
    pub fn purged_record(&self, asset_id: String) -> Result<Option<Vec<u8>>, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            ws.purge_record(&asset_id)
                .map(capsule_core::cbor::to_canonical_vec)
                .transpose()
                .map_err(|e| FfiError::Workspace {
                    detail: format!("encoding the purge record of {asset_id}: {e}"),
                })
        })
    }
}
