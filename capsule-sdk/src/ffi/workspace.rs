//! The **workspace verbs** of the `capsule_sdk` uniffi namespace (slice `S-P1`) — enroll,
//! create an album, seal + import an asset, verify, sync-apply, mint an escrow, publish a
//! directory — over `capsule-core`'s [`Workspace`].
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
//! [`Workspace`]: capsule_core::lifecycle::Workspace

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use capsule_core::crypto::keys::{
    HardwareBackedSigner, HardwareKeyAgreement, HardwareSigner, HardwareSignerError, P256HybridDek,
    P256HybridSigningKey, Signer, encapsulate_to_p256_public,
};
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::lifecycle::{
    QuarantineReason, RemoteAssetFacts, RemoteEntry, SyncApplyOutcome, Workspace,
};
use capsule_core::sidecar::SidecarV1;
use uuid::Uuid;

use super::{FfiError, FfiUploadRequest};

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
/// - [`FfiWorkspace::create_with_p256_hardware_signer`] — the **P-256** shape shipping
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
/// It backs the hardware-bound classical half of the device **encryption** key (slice `S-F5`):
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
        message: "ml_seed must be exactly 32 bytes".into(),
    })
}

/// Smoke the **hardware-bound P-256 device encryption key** (slice `S-F5`) across the FFI seam:
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
            message: format!("hardware P-256 DEK enrollment failed: {e}"),
        })?;
    let workspace_error = |e: capsule_core::crypto::CryptoError| FfiError::Workspace {
        message: e.to_string(),
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

/// A foreign client's self-reported build identity (`S-D15`): its product id and own semver.
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
    pub original_ciphertext: Vec<u8>,
    /// The 32-byte provenance head your catalog already holds for this asset, or `None` if it
    /// has never seen it. This is what decides replay and fork; feed back
    /// [`FfiAssetFacts::provenance_head`] after a successful apply.
    pub local_chain_head: Option<Vec<u8>>,
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
    /// RFC3339 capture time.
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
        Self {
            sidecar_schema: sidecar.sidecar_schema,
            content_type: sidecar.content_type,
            capture_timestamp: sidecar.capture_timestamp,
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
        /// continuation attributed to the wrong device of the same account; not a forgery).
        reason: String,
        /// English detail, when the refusal carries one (a parse message, an album id).
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
                };
                Self::Quarantined { reason, detail }
            }
        }
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
    fn with<T>(
        &self,
        f: impl FnOnce(&mut Workspace) -> Result<T, FfiError>,
    ) -> Result<T, FfiError> {
        let mut guard = self.inner.lock().map_err(|_| FfiError::Workspace {
            message: "workspace lock poisoned".into(),
        })?;
        f(&mut guard)
    }

    fn wrap(workspace: Workspace) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(workspace),
        })
    }
}

/// Parse a UUID from foreign code into a typed error rather than panicking at the boundary.
fn parse_uuid(field: &str, value: &str) -> Result<Uuid, FfiError> {
    Uuid::parse_str(value).map_err(|e| FfiError::InvalidArgument {
        message: format!("{field}: {value:?} is not a UUID ({e})"),
    })
}

/// Parse the feed's 16-byte UUID form.
fn uuid_from_bytes(field: &str, bytes: &[u8]) -> Result<Uuid, FfiError> {
    Uuid::from_slice(bytes).map_err(|e| FfiError::InvalidArgument {
        message: format!("{field}: expected 16 UUID bytes ({e})"),
    })
}

/// The ladder tier's stable lowercase name (`index` / `preview` / `original`) — the same three
/// rungs the transfer download tier ladder names, so an app's progress UI can key on one vocabulary
/// in both directions.
fn tier_name(tier: capsule_core::import::UploadTier) -> String {
    use capsule_core::import::UploadTier;
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
            message: format!("hardware Ed25519 key enrollment failed: {e}"),
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
    /// The published device key carries the P-256 tag and `verify_asset` dispatches on it, so
    /// this is constructor parity with `capsule-core`'s own
    /// `create_with_p256_hardware_signer` — the same core call, reached through this
    /// namespace's [`FfiHardwareSigner`] seam.
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
        let signer = P256HybridSigningKey::enroll(
            Arc::new(ForeignHardwareSigner(hardware)),
            key_alias,
            &seed,
        )
        .map_err(|e| FfiError::Workspace {
            message: format!("hardware P-256 key enrollment failed: {e}"),
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
            "workspace created with a hardware-bound P-256 device key"
        );
        Ok(Self::wrap(workspace))
    }

    /// **Open** the existing library at `root` under `passphrase`, restoring its album keys and
    /// authorities, every managed asset, and the `add_id` counter from disk (`S-A10`). This is
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
                        message: format!("reading the original of {asset_id}: {e}"),
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
        self.with(|ws| {
            Ok(ws
                .upload_bundle(&asset_id)
                .map_err(|e| FfiError::Workspace {
                    message: format!("reading the provenance head of {asset_id}: {e}"),
                })?
                .provenance_blob)
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
                message: format!("asset {asset_id} is not managed by this workspace"),
            })?;
            let head = asset
                .chain
                .records()
                .last()
                .ok_or_else(|| FfiError::Workspace {
                    message: format!("asset {asset_id} has an empty provenance chain"),
                })?;
            capsule_core::cbor::to_canonical_vec(&head.manifest).map_err(|e| FfiError::Workspace {
                message: format!("encoding the signed manifest failed: {e}"),
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
        let local_chain_head = entry
            .local_chain_head
            .as_deref()
            .map(|head| {
                <[u8; 32]>::try_from(head)
                    .map(capsule_core::crypto::hash::Hash32::from_bytes)
                    .map_err(|_| FfiError::InvalidArgument {
                        message: "local_chain_head must be exactly 32 bytes".into(),
                    })
            })
            .transpose()?;
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
                message: format!("encoding the escrow blob failed: {e}"),
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
                message: format!("escrow blob is not a canonical WrappedSecret: {e}"),
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
                    message: format!("encoding the device directory failed: {e}"),
                }
            })
        })
    }

    /// This account's user identity **public** key — the pin a fetched device directory is
    /// verified against.
    pub fn user_ik_public(&self) -> Result<Vec<u8>, FfiError> {
        self.with(|ws| Ok(ws.user_ik_public().to_bytes()))
    }
}
