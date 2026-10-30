//! uniffi bindings: an async-capable wrapper over the SDK's user-flow primitives, so iOS/macOS
//! (Swift), Android (Kotlin), and Linux consumers call
//! one SDK instead of reimplementing login/upload/sync flows.
//!
//! # Surface
//!
//! [`FfiCapsuleClient`] is the unauthenticated entry point (auth + upload endpoints
//! and the protocol pin). [`FfiCapsuleClient::login`]/[`register`](crate::ffi::FfiCapsuleClient::register)
//! return an [`FfiSession`] **handle** — an opaque object that owns the token store
//! internally. Every subsequent call ([`upload`](crate::ffi::FfiSession::upload),
//! [`push_asset`](crate::ffi::FfiSession::push_asset),
//! [`upload_status`](crate::ffi::FfiSession::upload_status),
//! [`fetch_blob_to_path`](crate::ffi::FfiSession::fetch_blob_to_path),
//! [`sync_pull`](crate::ffi::FfiSession::sync_pull),
//! [`logout`](crate::ffi::FfiSession::logout)) rides that handle, and
//! [`transfer_events`](crate::ffi::FfiSession::transfer_events) subscribes to the step-by-step
//! progress of the uploads and fetches it drives. **Tokens never cross the FFI
//! boundary raw**: the SDK session store injects and auto-refreshes the bearer
//! behind the handle, so foreign code never sees access/refresh token material.
//!
//! [`FfiCapsuleClient`]: crate::ffi::FfiCapsuleClient
//! [`FfiCapsuleClient::login`]: crate::ffi::FfiCapsuleClient::login
//! [`FfiSession`]: crate::ffi::FfiSession
//!
//! # Async
//!
//! The flows are inherently networked, so the exported methods are `async fn` under
//! `#[uniffi::export(async_runtime = "tokio")]`: uniffi drives the returned future on
//! the tokio runtime `reqwest`/`tonic` require, and foreign callers see idiomatic
//! `suspend` (Kotlin) / `async` (Swift) functions. The Rust methods stay plain
//! `async fn`, so the in-crate smoke exercises them with a direct `.await`.
//!
//! # Types
//!
//! Following `capsule-core`'s FFI philosophy, the surface is deliberately minimal:
//! ids/hashes/paths cross as `String`, blobs/manifests/cursors as `bytes`, and the
//! rich SDK enums/records are flattened into `Ffi*` mirrors so the Rust API stays
//! free to evolve behind the boundary. Errors carry the stable `error.*` catalog
//! `code` (which clients localize) plus the English `detail` (which stays
//! English) — mirroring the server's `{ error, code }` contract. The field is `detail`, not
//! `message`, because uniffi turns each variant into a Kotlin `Throwable` subclass whose own
//! `message` property a `val message` field would clash with.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use capsule_library::import::UploadPolicy;
use capsule_library::lifecycle::{LifecycleError, UploadBundle};
use secrecy::{ExposeSecret as _, SecretString};

use crate::auth::{AuthClient, AuthError, LoginOutcome, Session};
use crate::directory::{DirectoryClient, DirectoryError};
use crate::events::TransferEvents;
use crate::fetch::{FetchError, HttpBlobSource, RangedFetcher};
use crate::net::ConnectionClass;
use crate::net::path::{GenerationPool, PathMonitor, PathSample};
use crate::push::{AssetPushReport, PushError, PushedBlob};
use crate::recovery::{EscrowStored, RecoveryClient, RecoveryError};
use crate::staged::{TierSessionOutcome, run_under_path};
use crate::sync::{
    BlobManifest, BlobRef, ChangeKind, FeedEntry, SyncConsumer, SyncCursor, SyncError, SyncPage,
};
use crate::upload::queue::LaneWindows;
use crate::upload::{
    BlobRole, CreateUploadRequest, HeadInfo, ManifestEnvelope, UploadClient, UploadError,
    UploadOutcome, UploadTransport,
};

/// The `capsule-core`-facing workspace verbs: enroll/open, albums, seal + import,
/// verify, sync-apply, escrow minting, the signed device directory, and labels and regions. Separated from the
/// networked client/session surface below because it is the half that touches no transport.
mod workspace;

pub use workspace::{
    FfiAlbum, FfiAssetFacts, FfiAssetMetadata, FfiClientBuild, FfiDeviceTier, FfiFeedDrain,
    FfiHardwareKeyAgreement, FfiHardwareSigner, FfiHardwareSignerError, FfiLabelRefusal,
    FfiLabelTarget, FfiOpenedOriginal, FfiPurgeOutcome, FfiRegion, FfiRegionShape,
    FfiSyncApplyOutcome, FfiSyncEntry, FfiSyncHead, FfiTrashEntry, FfiTrashRefusal, FfiUploadBlob,
    FfiVerifyOutcome, FfiWorkspace, p256_hardware_dek_round_trip,
};

/// The cull verbs on [`FfiWorkspace`]: flag an asset or a stack, read a stack's derived state,
/// list by flag, list stack membership, and the reject sweep. Its own module because culling is one workflow with one
/// destructive step, and reads better as a unit than as another section of `workspace`.
mod culling;

pub use culling::{FfiCullFlag, FfiStackCullState, FfiStackMember};

/// The transfer event stream: the flattened mirror of `crate::events` and the subscription a
/// real-time transfer view polls. Separated because it is the one surface that is a stream
/// rather than a call.
mod events;

pub use events::{
    FfiRetryReason, FfiStampedTransferEvent, FfiTransferEvent, FfiTransferFeedItem,
    FfiTransferSubscription,
};

/// The local-alert surface: the shared `capsule_core::notify` predicate, flattened.
/// Separated from both blocks above because it touches neither a transport nor a workspace —
/// it is a pure function of caller-supplied device state, and the two free exports say so.
mod notify;

pub use notify::{
    FfiAlert, FfiAlertClass, FfiAlertSeverity, FfiClassDeadline, FfiNotifyInput, FfiQuotaAdvisory,
    evaluate_alerts, next_alert_deadline, pre_arm_deadlines,
};

/// The path snapshot an app's OS listeners report, flattened, and what a finished push tells
/// the adverse detector. Separated because it is the session's one input that is device state
/// rather than a request.
mod path;

pub use path::{FfiPathCost, FfiPathInterface, FfiPathSnapshot, FfiPowerState, FfiThermalState};

/// The render ladder: the one order every app renders by, and the embedded-preview read that
/// feeds it. Separated for the reason `notify` is: pure functions of caller-supplied state.
mod ladder;

pub use ladder::{
    FfiEmbeddedPreview, FfiLocalHoldings, FfiNextStep, FfiRenderRequest, FfiRenderStep,
    FfiRendition, FfiRepresentation, FfiRungSource, embedded_preview, render_step,
};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Every failure the FFI flows can surface, flattened by originating layer. Each
/// variant carries the stable `error.*` catalog `code` (when one applies — clients
/// localize it) and the English `detail` (stays English), mirroring the
/// SDK's `{ error, code }` contract so foreign apps switch on the code, never a
/// bare HTTP status.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// An authentication flow (login/register/refresh/logout) failed.
    #[error("authentication failed ({code:?}): {detail}")]
    Auth {
        /// Stable `error.*` catalog code, when the SDK mapped one.
        code: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// An upload flow (create/transfer/status) failed.
    #[error("upload failed ({code:?}): {detail}")]
    Upload {
        /// Stable `error.*` catalog code, when the server supplied one.
        code: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A sync flow (pull/reconcile) failed.
    #[error("sync failed ({code:?}): {detail}")]
    Sync {
        /// Stable `error.*` catalog code, when one applies.
        code: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A local workspace operation failed — opening the library, sealing an asset, reading a
    /// managed asset. This is the *workspace* failing, never an entry being refused: a refused
    /// sync entry is a [`FfiSyncApplyOutcome::Quarantined`] verdict, not an error.
    #[error("workspace error: {detail}")]
    Workspace {
        /// English detail (developer/log message).
        detail: String,
    },
    /// A label or region edit was refused before anything was written: its target does not
    /// exist, the label rule refused the label, or the shape broke the normalized-coordinate
    /// contract. `refusal` says which, so an editor shows the reason rather than parsing
    /// `detail`.
    #[error("label edit refused ({refusal:?}): {detail}")]
    Label {
        /// Why the edit was refused.
        refusal: FfiLabelRefusal,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A trash verb (delete, restore, purge) was refused before anything was written: the asset
    /// is unknown, not in the trash, or past its signed retention window. `refusal` says which.
    #[error("trash verb refused ({refusal:?}): {detail}")]
    Trash {
        /// Why the verb was refused.
        refusal: FfiTrashRefusal,
        /// Stable `error.*` catalog code, when one applies: `error.upload.retention_elapsed`
        /// for [`FfiTrashRefusal::RetentionElapsed`], the code the server answers the same
        /// refusal with.
        code: Option<String>,
        /// The signed retention floor (RFC 3339) a refused restore came too late for.
        retention_until: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A master-key escrow flow (store/fetch) failed.
    #[error("escrow failed ({code:?}): {detail}")]
    Escrow {
        /// Stable `error.*` catalog code, when the server supplied one — `error.escrow.*`
        /// separates "you have no recovery backup" (a setup prompt) from "we could not read
        /// it" (a retry), which is the whole reason the catalog distinguishes them.
        code: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A device-directory flow (publish/fetch) failed.
    #[error("device directory failed ({code:?}): {detail}")]
    Directory {
        /// Stable `error.*` catalog code, when one applies.
        code: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A blob fetch ([`FfiSession::fetch_blob_to_path`]) failed: the blob is gone, still
    /// awaiting its original, refused, did not hash to its content address, or could not be
    /// written locally. Nothing is left at the destination path.
    #[error("blob fetch failed ({code:?}): {detail}")]
    Fetch {
        /// Stable `error.*` catalog code, when one applies (e.g. `error.blob.pending_upload`).
        code: Option<String>,
        /// English detail (developer/log message).
        detail: String,
    },
    /// A foreign argument was structurally invalid (e.g. an un-parseable URL) before
    /// any network call.
    #[error("invalid argument: {detail}")]
    InvalidArgument {
        /// What was wrong with the argument.
        detail: String,
    },
}

impl From<LifecycleError> for FfiError {
    fn from(err: LifecycleError) -> Self {
        Self::Workspace {
            detail: err.to_string(),
        }
    }
}

impl From<RecoveryError> for FfiError {
    fn from(err: RecoveryError) -> Self {
        // A refused credential under an escrow call keeps its auth identity so callers can
        // trigger interactive re-authentication, exactly as the upload mapping does. Before
        // the escrow calls moved onto the generated client this arrived as
        // `RecoveryError::Auth`; routing `Unauthorized` here is what stops the move from
        // silently downgrading "sign in again" into "escrow failed".
        if let RecoveryError::Unauthorized { code, detail } = err {
            return Self::Auth { code, detail };
        }
        // A malformed argument is the caller's, not the escrow surface's.
        if let RecoveryError::InvalidBaseUrl { .. } = err {
            return Self::InvalidArgument {
                detail: err.to_string(),
            };
        }
        Self::Escrow {
            code: err.error_code().map(str::to_owned),
            detail: err.to_string(),
        }
    }
}

impl From<DirectoryError> for FfiError {
    fn from(err: DirectoryError) -> Self {
        if let DirectoryError::Auth(auth) = err {
            return auth.into();
        }
        Self::Directory {
            code: err.error_code().map(str::to_string),
            detail: err.to_string(),
        }
    }
}

impl From<AuthError> for FfiError {
    fn from(err: AuthError) -> Self {
        let code = err.error_code().map(str::to_string);
        match &err {
            AuthError::InvalidBaseUrl { .. } => Self::InvalidArgument {
                detail: err.to_string(),
            },
            _ => Self::Auth {
                code,
                detail: err.to_string(),
            },
        }
    }
}

impl From<UploadError> for FfiError {
    fn from(err: UploadError) -> Self {
        // Auth failures under an upload keep their auth identity so callers can
        // trigger interactive re-authentication.
        if let UploadError::Auth(auth) = err {
            return auth.into();
        }
        let code = match &err {
            UploadError::UpgradeRequired { .. } => {
                Some(capsule_i18n::error_codes::PROTOCOL_VERSION_UNSUPPORTED.to_owned())
            }
            _ => err.error_code().map(str::to_owned),
        };
        Self::Upload {
            code,
            detail: err.to_string(),
        }
    }
}

impl From<PushError> for FfiError {
    fn from(err: PushError) -> Self {
        // A transfer failure keeps the upload mapping (and with it the auth identity of a
        // refused credential); everything else a push can say is an upload-flow failure.
        match err {
            PushError::Upload(upload) => return upload.into(),
            PushError::CreateRefused { source, .. } => return (*source).into(),
            _ => {}
        }
        Self::Upload {
            code: err.error_code().map(str::to_string),
            detail: err.to_string(),
        }
    }
}

impl From<FetchError> for FfiError {
    fn from(err: FetchError) -> Self {
        // A fetch the session could not authorize keeps its auth identity, as the upload and
        // sync mappings do, so the app re-authenticates rather than retrying.
        if let FetchError::Unauthenticated { code, message } = err {
            return Self::Auth {
                code,
                detail: message,
            };
        }
        Self::Fetch {
            code: err.error_code().map(str::to_string),
            detail: err.to_string(),
        }
    }
}

impl From<SyncError> for FfiError {
    fn from(err: SyncError) -> Self {
        if let SyncError::Auth(auth) = err {
            return auth.into();
        }
        let code = err.error_code().map(str::to_string);
        Self::Sync {
            code,
            detail: err.to_string(),
        }
    }
}

// ─── Upload request mirrors ──────────────────────────────────────────────────

/// FFI mirror of [`BlobRole`] (snake_case on the wire, closed enum).
#[derive(uniffi::Enum)]
pub enum FfiBlobRole {
    /// The source asset.
    Original,
    /// A client-generated thumbnail/preview.
    Derivative,
    /// The CBOR metadata document.
    Metadata,
    /// Append-only provenance.
    Provenance,
    /// A library backup artifact.
    Backup,
    /// A derivative tier's signed manifest.
    DerivativeManifest,
}

impl From<FfiBlobRole> for BlobRole {
    fn from(role: FfiBlobRole) -> Self {
        match role {
            FfiBlobRole::Original => Self::Original,
            FfiBlobRole::Derivative => Self::Derivative,
            FfiBlobRole::Metadata => Self::Metadata,
            FfiBlobRole::Provenance => Self::Provenance,
            FfiBlobRole::Backup => Self::Backup,
            FfiBlobRole::DerivativeManifest => Self::DerivativeManifest,
        }
    }
}

impl From<BlobRole> for FfiBlobRole {
    fn from(role: BlobRole) -> Self {
        match role {
            BlobRole::Original => Self::Original,
            BlobRole::Derivative => Self::Derivative,
            BlobRole::Metadata => Self::Metadata,
            BlobRole::Provenance => Self::Provenance,
            BlobRole::Backup => Self::Backup,
            BlobRole::DerivativeManifest => Self::DerivativeManifest,
        }
    }
}

/// FFI mirror of [`ManifestEnvelope`] — the server-visible envelope fields the
/// import pipeline builds and signs; opaque to this layer, carried verbatim.
#[derive(uniffi::Record)]
pub struct FfiManifestEnvelope {
    /// Crypto suite the blob is sealed under.
    pub crypto_suite_id: u16,
    /// Protocol date (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// Optional album this blob belongs to.
    pub album_id: Option<String>,
    /// The asset id this blob belongs to (UUIDv7).
    pub file_id: String,
    /// AMK epoch the blob is sealed under.
    pub amk_version: u32,
    /// Ciphertext content hash, lowercase hex.
    pub ciphertext_hash: String,
    /// Plaintext size in bytes.
    pub plaintext_size: u64,
    /// STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// `derived | wrapped`.
    pub key_mode: String,
    /// Hash of the bundle's metadata blob, when known.
    pub metadata_blob_hash: Option<String>,
    /// A derivative blob's role (`thumbnail | preview | embedding`); `None` for every other
    /// blob.
    pub derivative_role: Option<String>,
    /// A derivative blob's format, e.g. `image/jxl`; set exactly when `derivative_role` is.
    pub derivative_format: Option<String>,
    /// The authoring user.
    pub created_by_user: String,
    /// The authoring device.
    pub created_by_device: String,
    /// The exact client build.
    pub client_version: String,
    /// RFC 3339 authoring timestamp.
    pub timestamp: String,
    /// Lifecycle action (`create` for a fresh bundle).
    pub action: String,
    /// Prior provenance hash, for append-only chains.
    pub prior_provenance_hash: Option<String>,
    /// Optional retention floor.
    pub retention_until: Option<String>,
}

impl From<FfiManifestEnvelope> for ManifestEnvelope {
    fn from(env: FfiManifestEnvelope) -> Self {
        Self {
            crypto_suite_id: env.crypto_suite_id,
            protocol_version: env.protocol_version,
            album_id: env.album_id,
            file_id: env.file_id,
            amk_version: env.amk_version,
            ciphertext_hash: env.ciphertext_hash,
            plaintext_size: env.plaintext_size,
            chunk_size: env.chunk_size,
            key_mode: env.key_mode,
            metadata_blob_hash: env.metadata_blob_hash,
            derivative_role: env.derivative_role,
            derivative_format: env.derivative_format,
            created_by_user: env.created_by_user,
            created_by_device: env.created_by_device,
            client_version: env.client_version,
            timestamp: env.timestamp,
            action: env.action,
            prior_provenance_hash: env.prior_provenance_hash,
            retention_until: env.retention_until,
        }
    }
}

impl From<ManifestEnvelope> for FfiManifestEnvelope {
    fn from(env: ManifestEnvelope) -> Self {
        Self {
            crypto_suite_id: env.crypto_suite_id,
            protocol_version: env.protocol_version,
            album_id: env.album_id,
            file_id: env.file_id,
            amk_version: env.amk_version,
            ciphertext_hash: env.ciphertext_hash,
            plaintext_size: env.plaintext_size,
            chunk_size: env.chunk_size,
            key_mode: env.key_mode,
            metadata_blob_hash: env.metadata_blob_hash,
            derivative_role: env.derivative_role,
            derivative_format: env.derivative_format,
            created_by_user: env.created_by_user,
            created_by_device: env.created_by_device,
            client_version: env.client_version,
            timestamp: env.timestamp,
            action: env.action,
            prior_provenance_hash: env.prior_provenance_hash,
            retention_until: env.retention_until,
        }
    }
}

/// FFI mirror of [`CreateUploadRequest`] — the `POST /upload` body.
#[derive(uniffi::Record)]
pub struct FfiUploadRequest {
    /// Ciphertext size in bytes.
    pub size: u64,
    /// Ciphertext content hash, lowercase hex.
    pub hash: String,
    /// MIME type (closed enum per protocol version).
    pub content_type: String,
    /// Crypto suite the blob is sealed under.
    pub crypto_suite_id: u16,
    /// Protocol date (`YYYY-MM-DD`) this client speaks.
    pub protocol_version: String,
    /// This blob's role in its bundle.
    pub blob_role: FfiBlobRole,
    /// The unencrypted manifest fields the server validates.
    pub manifest_envelope: FfiManifestEnvelope,
    /// Optional album to add the asset to.
    pub album_id: Option<String>,
    /// Optional owner id (defaults to the authenticated uploader).
    pub owner_id: Option<String>,
    /// Album-upgrade intent id (album upgrade ceremony only).
    pub intent_id: Option<String>,
}

impl From<FfiUploadRequest> for CreateUploadRequest {
    fn from(req: FfiUploadRequest) -> Self {
        Self {
            size: req.size,
            hash: req.hash,
            content_type: req.content_type,
            crypto_suite_id: req.crypto_suite_id,
            protocol_version: req.protocol_version,
            blob_role: req.blob_role.into(),
            manifest_envelope: req.manifest_envelope.into(),
            album_id: req.album_id,
            owner_id: req.owner_id,
            intent_id: req.intent_id,
        }
    }
}

/// The reverse mapping, so [`FfiWorkspace::upload_blobs`] can hand back exactly what
/// [`FfiSession::upload`] takes. It reuses `capsule_sdk::push`'s envelope projection wholesale
/// — the invariant-15 per-blob `ciphertext_hash` rule has one implementation, in `push`, and
/// this is a shape conversion over its output.
impl From<CreateUploadRequest> for FfiUploadRequest {
    fn from(req: CreateUploadRequest) -> Self {
        Self {
            size: req.size,
            hash: req.hash,
            content_type: req.content_type,
            crypto_suite_id: req.crypto_suite_id,
            protocol_version: req.protocol_version,
            blob_role: req.blob_role.into(),
            manifest_envelope: req.manifest_envelope.into(),
            album_id: req.album_id,
            owner_id: req.owner_id,
            intent_id: req.intent_id,
        }
    }
}

// ─── Upload outcome / status mirrors ─────────────────────────────────────────

/// FFI mirror of [`UploadOutcome`] — how a full upload resolved.
#[derive(uniffi::Enum)]
pub enum FfiUploadOutcome {
    /// Every declared byte transferred; the server has begun finalization.
    Completed {
        /// The session driven to completion.
        session_id: String,
    },
    /// The blob was already stored server-side (the `duplicate_blob` → merge path).
    AlreadyStored {
        /// The existing asset the caller merges onto.
        asset_ref: String,
    },
}

impl From<UploadOutcome> for FfiUploadOutcome {
    fn from(outcome: UploadOutcome) -> Self {
        match outcome {
            UploadOutcome::Completed { session_id } => Self::Completed { session_id },
            UploadOutcome::AlreadyStored { asset_ref } => Self::AlreadyStored { asset_ref },
        }
    }
}

/// FFI mirror of [`HeadInfo`] — the `HEAD /upload/{id}` resumption/status primitive.
#[derive(uniffi::Record)]
pub struct FfiUploadStatus {
    /// Next expected byte (authoritative received-byte count).
    pub offset: u64,
    /// Declared total size, when known.
    pub total_size: Option<u64>,
    /// Session state (`X-Capsule-Upload-Status`).
    pub status: String,
}

impl From<HeadInfo> for FfiUploadStatus {
    fn from(info: HeadInfo) -> Self {
        Self {
            offset: info.offset,
            total_size: info.total_size,
            status: info.status,
        }
    }
}

// ─── Push report mirrors ─────────────────────────────────────────────────────

/// How one blob of a pushed asset resolved — FFI mirror of [`PushedBlob`].
#[derive(uniffi::Record)]
pub struct FfiPushedBlob {
    /// The ladder tier the blob occupies (`index` / `preview` / `original`), the vocabulary
    /// [`FfiUploadBlob::tier`] uses.
    pub tier: String,
    /// The blob's content address (lowercase hex).
    pub hash: String,
    /// Whether it transferred or merged onto a blob the server already held.
    pub outcome: FfiUploadOutcome,
}

impl From<PushedBlob> for FfiPushedBlob {
    fn from(blob: PushedBlob) -> Self {
        Self {
            tier: workspace::tier_name(blob.tier),
            hash: blob.hash,
            outcome: match blob.outcome {
                TierSessionOutcome::Uploaded { session_id } => {
                    FfiUploadOutcome::Completed { session_id }
                }
                TierSessionOutcome::AlreadyStored { asset_ref } => {
                    FfiUploadOutcome::AlreadyStored { asset_ref }
                }
            },
        }
    }
}

/// What [`FfiSession::push_asset`] did — FFI mirror of [`AssetPushReport`].
#[derive(uniffi::Record)]
pub struct FfiPushReport {
    /// The asset id (UUID string).
    pub asset_id: String,
    /// Every blob whose session was opened, in ladder order.
    pub pushed: Vec<FfiPushedBlob>,
    /// Total bytes handed to the upload client, the original included.
    pub bytes: u64,
    /// Blobs the reported path held back ([`FfiSession::set_path`]): push the asset again once
    /// the path allows them. Zero when the whole ladder ran.
    pub deferred: u32,
}

impl From<AssetPushReport> for FfiPushReport {
    fn from(report: AssetPushReport) -> Self {
        Self {
            asset_id: report.asset_id,
            pushed: report.pushed.into_iter().map(FfiPushedBlob::from).collect(),
            bytes: report.bytes,
            deferred: u32::try_from(report.deferred).unwrap_or(u32::MAX),
        }
    }
}

// ─── Sync mirrors ────────────────────────────────────────────────────────────

/// FFI mirror of [`ChangeKind`].
#[derive(uniffi::Enum)]
pub enum FfiChangeKind {
    /// A new asset became visible in the album.
    Created,
    /// An existing asset advanced — new metadata, new bytes, or a restore.
    Updated,
    /// The asset was tombstoned.
    Deleted,
}

impl From<ChangeKind> for FfiChangeKind {
    fn from(kind: ChangeKind) -> Self {
        match kind {
            ChangeKind::Created => Self::Created,
            ChangeKind::Updated => Self::Updated,
            ChangeKind::Deleted => Self::Deleted,
        }
    }
}

/// FFI mirror of [`BlobRef`] — a blob's content address and role.
#[derive(uniffi::Record)]
pub struct FfiBlobRef {
    /// Ciphertext content address (lowercase hex).
    pub ciphertext_hash: String,
    /// `original | metadata | derivative | provenance`.
    pub role: String,
    /// Ciphertext size in bytes.
    pub size: u64,
}

impl From<BlobRef> for FfiBlobRef {
    fn from(blob: BlobRef) -> Self {
        Self {
            ciphertext_hash: blob.ciphertext_hash,
            role: blob.role,
            size: blob.size,
        }
    }
}

/// FFI mirror of [`BlobManifest`] — an asset's blobs by role.
#[derive(uniffi::Record)]
pub struct FfiBlobManifest {
    /// The original ciphertext blob, when this entry carries one.
    pub original: Option<FfiBlobRef>,
    /// Derivative / metadata / provenance blobs.
    pub derivatives: Vec<FfiBlobRef>,
}

impl From<BlobManifest> for FfiBlobManifest {
    fn from(manifest: BlobManifest) -> Self {
        Self {
            original: manifest.original.map(FfiBlobRef::from),
            derivatives: manifest
                .derivatives
                .into_iter()
                .map(FfiBlobRef::from)
                .collect(),
        }
    }
}

/// FFI mirror of [`FeedEntry`] — one decoded sync feed entry. Ids/manifest/metadata
/// cross as opaque bytes.
#[derive(uniffi::Record)]
pub struct FfiFeedEntry {
    /// The album this entry belongs to.
    pub album_id: Vec<u8>,
    /// Per-album, strictly-increasing anti-rewind high-water mark.
    pub sync_seq: u64,
    /// The album protocol pin (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// What changed.
    pub kind: FfiChangeKind,
    /// The asset id.
    pub asset_id: Vec<u8>,
    /// The signed `AssetManifest` as opaque canonical CBOR.
    pub manifest_cbor: Vec<u8>,
    /// The encrypted metadata blob's content address, as UTF-8 bytes; empty for deletes.
    pub metadata_blob: Vec<u8>,
    /// Content addresses of the asset's blobs by role.
    pub blobs: FfiBlobManifest,
    /// Whether the original blob is finalized server-side.
    pub original_held: bool,
    /// When the change happened, RFC 3339, on the server's clock.
    pub changed_at: String,
}

impl From<FeedEntry> for FfiFeedEntry {
    fn from(entry: FeedEntry) -> Self {
        Self {
            album_id: entry.album_id,
            sync_seq: entry.sync_seq,
            protocol_version: entry.protocol_version,
            kind: entry.kind.into(),
            asset_id: entry.asset_id,
            manifest_cbor: entry.manifest_cbor,
            metadata_blob: entry.metadata_blob,
            blobs: entry.blobs.into(),
            original_held: entry.original_held,
            changed_at: entry.changed_at,
        }
    }
}

/// FFI mirror of [`SyncPage`] — a decoded page plus the opaque cursor for the next
/// pull (the client persists `next_cursor` and passes it back to advance).
#[derive(uniffi::Record)]
pub struct FfiSyncPage {
    /// The changes in this page, in strictly-increasing per-album `sync_seq` order.
    pub entries: Vec<FfiFeedEntry>,
    /// The opaque cursor to pass to the next [`FfiSession::sync_pull`].
    pub next_cursor: Vec<u8>,
    /// Whether the server holds changes beyond this page.
    pub has_more: bool,
}

impl From<SyncPage> for FfiSyncPage {
    fn from(page: SyncPage) -> Self {
        Self {
            entries: page.entries.into_iter().map(FfiFeedEntry::from).collect(),
            next_cursor: page.next_cursor.as_bytes().to_vec(),
            has_more: page.has_more,
        }
    }
}

/// FFI mirror of [`EscrowStored`] — what [`FfiSession::escrow_put`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEscrowStored {
    /// When the server accepted the blob, RFC 3339, on the server's clock.
    pub stored_at: String,
    /// Whether this displaced an earlier escrow: `false` is a first enrollment (recovery set
    /// up), `true` a replacement (the previous recovery secret has stopped working).
    pub replaced: bool,
}

impl From<EscrowStored> for FfiEscrowStored {
    fn from(stored: EscrowStored) -> Self {
        Self {
            stored_at: stored.stored_at.to_string(),
            replaced: stored.replaced,
        }
    }
}

// ─── Client (entry point) ────────────────────────────────────────────────────

/// The unauthenticated entry point over the SDK's auth + upload endpoints. Turns
/// credentials into an [`FfiSession`] handle; the protocol pin and endpoints are
/// captured once here and reused by every session this client mints.
#[derive(uniffi::Object)]
pub struct FfiCapsuleClient {
    auth: AuthClient,
    upload_base_url: String,
    protocol_version: String,
}

#[uniffi::export]
impl FfiCapsuleClient {
    /// Build a client.
    ///
    /// - `auth_base_url` is the auth endpoint root (e.g. `https://api.example.com/auth`).
    /// - `upload_base_url` is the upload endpoint root (`POST` target; chunk/HEAD hang
    ///   under `{upload_base_url}/{id}`).
    /// - `protocol_version` is the pinned protocol date (`YYYY-MM-DD`).
    /// - `cohort_hash` is the optional advisory device-cohort digest; `None`
    ///   sends nothing (the server behaves identically).
    #[uniffi::constructor]
    pub fn new(
        auth_base_url: String,
        upload_base_url: String,
        protocol_version: String,
        cohort_hash: Option<String>,
    ) -> Result<Arc<Self>, FfiError> {
        let mut auth = AuthClient::new(&auth_base_url)?;
        if let Some(cohort) = cohort_hash {
            auth = auth.with_cohort_hash(cohort);
        }
        Ok(Arc::new(Self {
            auth,
            upload_base_url: upload_base_url.trim_end_matches('/').to_string(),
            protocol_version,
        }))
    }
}

/// What a password login answered with, across the FFI.
///
/// An enum rather than an optional session handle: "signed in" and "needs a code" are different
/// states an app renders differently, and a `null` handle beside a `mfaToken` string would let a
/// caller read one while acting on the other.
#[derive(uniffi::Enum)]
pub enum FfiLoginOutcome {
    /// The account has no second factor, and this is its session.
    Session {
        /// The authenticated session handle.
        session: Arc<FfiSession>,
    },
    /// The password verified and a code is still needed.
    SecondFactorRequired {
        /// The challenge to hand to `verifySecondFactor`.
        mfa_token: String,
        /// The absolute Unix-seconds instant the challenge stops being honoured.
        expires_by: u64,
    },
}

#[uniffi::export(async_runtime = "tokio")]
impl FfiCapsuleClient {
    /// Authenticate with email + password, returning an authenticated session handle.
    pub async fn login(
        &self,
        email: String,
        password: String,
    ) -> Result<FfiLoginOutcome, FfiError> {
        match self.auth.login(&email, &password).await? {
            LoginOutcome::Session(session) => Ok(FfiLoginOutcome::Session {
                session: self.session_handle(session),
            }),
            // The password verified and the sign-in is not finished. No session
            // handle is produced, because there is no session — an app that got one here would
            // hold a handle to a half-authentication.
            LoginOutcome::SecondFactorRequired {
                mfa_token,
                expires_by,
            } => Ok(FfiLoginOutcome::SecondFactorRequired {
                mfa_token: mfa_token.expose_secret().to_owned(),
                expires_by,
            }),
        }
    }

    /// Complete a sign-in with the code an authenticator app is showing.
    ///
    /// `mfa_token` is what [`FfiLoginOutcome::SecondFactorRequired`] carried. It is good once
    /// and for five minutes.
    pub async fn verify_second_factor(
        &self,
        mfa_token: String,
        totp_code: String,
    ) -> Result<Arc<FfiSession>, FfiError> {
        let session = self
            .auth
            .verify_second_factor(&SecretString::from(mfa_token), &totp_code)
            .await?;
        Ok(self.session_handle(session))
    }

    /// Create an account and return an authenticated session handle (the server
    /// issues tokens on registration).
    /// An address and a password, and nothing else: the server takes nothing else.
    /// A display name is a fact about a person, and the profile surface that would hold one is
    /// owed rather than assumed.
    pub async fn register(
        &self,
        email: String,
        password: String,
    ) -> Result<Arc<FfiSession>, FfiError> {
        let session = self.auth.register(&email, &password).await?;
        Ok(self.session_handle(session))
    }
}

impl FfiCapsuleClient {
    fn session_handle(&self, session: Session) -> Arc<FfiSession> {
        Arc::new(FfiSession {
            session,
            upload_base_url: self.upload_base_url.clone(),
            protocol_version: self.protocol_version.clone(),
            events: TransferEvents::default_channel(),
            path: PathMonitor::new(PathSample::desktop(), LaneWindows::default()),
            pool: GenerationPool::sdk(),
        })
    }
}

// ─── Session (authenticated handle) ──────────────────────────────────────────

/// An authenticated session handle. Owns the SDK token store internally (auto
/// pre-flight refresh, single-flight, `401`-retry-once); **no token ever crosses
/// the FFI boundary**. Every flow rides this handle.
#[derive(uniffi::Object)]
pub struct FfiSession {
    session: Session,
    upload_base_url: String,
    protocol_version: String,
    /// Where every upload, push and fetch this session drives publishes its steps. Nothing is
    /// built while nobody is subscribed.
    events: TransferEvents,
    /// The path the app last reported and the link behaviour seen on it: every push and fetch
    /// plans from it. Until the app reports one, the desktop default — reachable, unmetered,
    /// foreground.
    path: PathMonitor,
    /// The HTTP client uploads ride, rebuilt on each new path generation so no pooled
    /// connection outlives its path.
    pool: GenerationPool,
}

#[uniffi::export]
impl FfiSession {
    /// Report the path as the app's OS listeners see it now, and return its path generation.
    ///
    /// Call it whenever a listener fires. The generation advances only when reachability, the
    /// interface or the cost changes; it then drops this session's pooled connections, forgets
    /// the resets and stalls seen on the old path, and cancels — within one scheduling tick —
    /// any push work the new path forbids, which re-plans and resumes. Power, thermal and
    /// background-deadline changes re-plan without a new generation.
    pub fn set_path(&self, snapshot: FfiPathSnapshot) -> u64 {
        let update = self.path.observe(snapshot.into());
        update.plan().generation.0
    }

    /// Subscribe to the step-by-step progress of every upload, push and fetch this session
    /// drives from now on, each event stamped with the milliseconds since this call on a
    /// monotonic clock.
    ///
    /// Progress is advisory: a transfer never waits on its observer, so a subscriber that
    /// falls behind reads [`FfiTransferFeedItem::Lagged`] with the count it missed. The
    /// subscription ends once this session is dropped.
    pub fn transfer_events(&self) -> Arc<FfiTransferSubscription> {
        FfiTransferSubscription::open(&self.events)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl FfiSession {
    /// Whether the session currently holds tokens (`false` after logout).
    pub async fn is_authenticated(&self) -> bool {
        self.session.is_authenticated().await
    }

    /// Upload a full ciphertext blob (`data`) under `request`, driving the adaptive,
    /// resumable protocol with the code-driven recovery matrix.
    ///
    /// `data` crosses the boundary whole, so this is for blobs an app already holds in memory.
    /// To push a sealed asset, original included, use [`push_asset`](Self::push_asset), which
    /// never holds the original whole.
    pub async fn upload(
        &self,
        request: FfiUploadRequest,
        data: Vec<u8>,
    ) -> Result<FfiUploadOutcome, FfiError> {
        let request: CreateUploadRequest = request.into();
        let outcome = self.upload_client().upload(&request, &data).await?;
        Ok(outcome.into())
    }

    /// Query an upload session's progress (`HEAD /upload/{id}`). `None` means the
    /// session is gone (discarded/expired/never-existent).
    pub async fn upload_status(
        &self,
        session_id: String,
    ) -> Result<Option<FfiUploadStatus>, FfiError> {
        let info = self.upload_client().head(&session_id).await?;
        Ok(info.map(FfiUploadStatus::from))
    }

    /// **Push** one sealed asset of `workspace` — every blob of its bundle, in ladder order
    /// (provenance and metadata, then derivative manifests and derivatives, then the
    /// original) — driving `capsule_sdk::push::push_bundle` inside the SDK.
    ///
    /// The original never crosses the boundary: its ciphertext is re-derived from the
    /// plaintext on disk one upload chunk window at a time, so memory is bounded by the
    /// transfer window whatever the original's size. The workspace lock is held only while the
    /// bundle is built, never across the network.
    ///
    /// Every blob is driven; one the server already holds answers `duplicate_blob` and comes
    /// back [`FfiUploadOutcome::AlreadyStored`], so pushing twice is safe. The album must
    /// already be provisioned on the server.
    ///
    /// **A create the server refused is re-sealed and retried once** (issue #822). When the
    /// album's epoch moved past the one the asset was sealed under, the server refuses its
    /// create for good; if this device holds a newer epoch, the asset is re-sealed at it
    /// (`Workspace::reseal_create`) and pushed again, once. A second refusal fails the push.
    /// When this device holds no newer epoch the push fails with [`FfiError::Upload`] carrying
    /// the server's code and a detail saying there is no newer epoch to re-seal at, and the
    /// asset stays local. That detail names no missing key: the refusal may have had nothing to
    /// do with the epoch (a revoked device, withdrawn write access), or another device rotated
    /// the album and its key has not reached this one yet.
    pub async fn push_asset(
        &self,
        workspace: Arc<FfiWorkspace>,
        asset_id: String,
    ) -> Result<FfiPushReport, FfiError> {
        let bundle = workspace.bundle(&asset_id)?;
        let refused = match self.push_bundle(&bundle).await {
            Err(PushError::CreateRefused { source, .. }) => source,
            other => return Ok(other?.into()),
        };
        tracing::warn!(
            asset_id = %bundle.asset_id,
            amk_version = bundle.amk_version,
            "ffi: the server refused this create; re-sealing it at the album's current epoch"
        );
        workspace.reseal(&bundle.asset_id, &refused)?;
        let resealed = workspace.bundle(&asset_id)?;
        let report = self.push_bundle(&resealed).await?;
        Ok(report.into())
    }

    /// **Fetch** the blob at content address `hash` (`size` bytes, as the sync feed's
    /// [`FfiBlobRef`] declares) into the file at `path`, created or truncated.
    ///
    /// The blob streams to disk in bounded `Range` windows, so memory is bounded by one window
    /// whatever the blob's size, and its content address is checked as it lands. On any error
    /// the file is removed: a path this returns `Ok` for holds exactly the verified ciphertext.
    /// A session that cannot authorize the fetch is [`FfiError::Auth`], so the app signs in
    /// again; every other failure is [`FfiError::Fetch`].
    ///
    /// `blob_base_url` is the root the blob route hangs off (`{blob_base_url}/blob/{hash}`),
    /// the server's `/v1` root.
    pub async fn fetch_blob_to_path(
        &self,
        blob_base_url: String,
        hash: String,
        size: u64,
        path: String,
    ) -> Result<(), FfiError> {
        if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(FfiError::InvalidArgument {
                detail: format!("hash: {hash:?} is not a lowercase hex SHA-256"),
            });
        }
        let path = PathBuf::from(path);
        tracing::info!(%hash, size, path = %path.display(), "ffi: fetching a blob to disk");
        let source = HttpBlobSource::new(self.path_session(), blob_base_url);
        // A fetch is a request the user is waiting on: the path sizes its range windows (bounded
        // under `adverse`) but never holds it back.
        let class = self.path.plan().class;
        let result = fetch_to_file(&source, &hash, size, &path, class, &self.events).await;
        if let Err(error) = &result {
            tracing::warn!(%hash, %error, "ffi: blob fetch failed; removing the partial file");
            // Best effort: the file may never have been created.
            let _ = std::fs::remove_file(&path);
        }
        result
    }

    /// Pull one page of the key-free sync feed after `cursor` from `api_base_url`
    /// (`http[s]://host:port`). Pass an empty `cursor` for the first page; persist and pass
    /// back `next_cursor` to advance. The bearer rides the `Authorization` header
    /// (auto-refreshed); it never crosses the FFI boundary.
    ///
    /// The parameter is the **API base URL** now, not a gRPC endpoint: the feed is
    /// `GET /v1/sync` on the same origin as every other call.
    pub async fn sync_pull(
        &self,
        api_base_url: String,
        cursor: Vec<u8>,
        page_size: u32,
    ) -> Result<FfiSyncPage, FfiError> {
        let consumer = SyncConsumer::with_session(&api_base_url, self.session.clone())?;
        let cursor = SyncCursor::from_bytes(cursor);
        let page = consumer.pull(&cursor, page_size).await?;
        Ok(page.into())
    }

    /// Store or replace this account's **master-key escrow blob** (`PUT /v1/auth/escrow`).
    /// `blob` is the opaque canonical CBOR
    /// [`FfiWorkspace::escrow_blob`](FfiWorkspace::escrow_blob) minted — the master key itself
    /// never crosses this boundary in either direction.
    ///
    /// Single active escrow: the server replaces any prior blob in the same transaction, so the
    /// secret a rotation retired unwraps nothing.
    ///
    /// `api_base_url` is the API root the session authenticates against (the per-call endpoint
    /// convention this surface already uses for `sync_pull`). A URL operation paths cannot hang
    /// off is [`FfiError::InvalidArgument`]; a refused credential is [`FfiError::Auth`], so a
    /// caller re-authenticates rather than retrying; everything else is
    /// [`FfiError::Escrow`] with the server's `error.escrow.*` code when it sent one.
    ///
    /// The answer says what the store did: [`FfiEscrowStored::replaced`] separates "recovery
    /// set up" from "recovery key replaced" (the previous secret has stopped working).
    pub async fn escrow_put(
        &self,
        api_base_url: String,
        blob: Vec<u8>,
    ) -> Result<FfiEscrowStored, FfiError> {
        let blob =
            capsule_core::cbor::from_slice(&blob).map_err(|e| FfiError::InvalidArgument {
                detail: format!("escrow blob is not a canonical WrappedSecret: {e}"),
            })?;
        let stored = RecoveryClient::new(self.session.clone(), &api_base_url)?
            .store_escrow(&blob)
            .await?;
        Ok(stored.into())
    }

    /// Fetch this account's escrow blob (`GET /v1/auth/escrow`) as opaque canonical CBOR — the
    /// bytes [`FfiWorkspace::verify_escrow_blob`](FfiWorkspace::verify_escrow_blob) checks and
    /// a recovery flow unwraps.
    ///
    /// No escrow enrolled yet is [`FfiError::Escrow`] carrying `error.escrow.not_stored` — the
    /// code that separates "set up a recovery key" from "we could not read the one you have".
    /// A refused credential is [`FfiError::Auth`].
    pub async fn escrow_get(&self, api_base_url: String) -> Result<Vec<u8>, FfiError> {
        let cache = RecoveryClient::new(self.session.clone(), &api_base_url)?
            .fetch_escrow()
            .await?;
        capsule_core::cbor::to_canonical_vec(cache.blob()).map_err(|e| FfiError::Escrow {
            // A local encode failure is ours, not the server's: no catalog code applies.
            code: None,
            detail: format!("encoding the fetched escrow blob failed: {e}"),
        })
    }

    /// Publish this device's **signed device directory**, returning the `directory_version` the
    /// server now stores. `directory_cbor` is the opaque document
    /// [`FfiWorkspace::signed_device_directory`](FfiWorkspace::signed_device_directory)
    /// produced; it travels verbatim, because re-encoding it would detach it from the signature
    /// it carries. `user_ik` is the account's user identity **public** key the document is
    /// signed under — the bytes [`FfiWorkspace::user_ik_public`](FfiWorkspace::user_ik_public)
    /// returns — which the server anchors on first publish and verifies every publish against.
    ///
    /// Publish when the document changes (a device enrolled or revoked). The version must
    /// advance — republishing an unchanged document answers the version-conflict code.
    pub async fn publish_device_directory(
        &self,
        api_base_url: String,
        directory_cbor: Vec<u8>,
        user_ik: Vec<u8>,
    ) -> Result<u64, FfiError> {
        let directory = capsule_core::cbor::from_slice(&directory_cbor).map_err(|e| {
            FfiError::InvalidArgument {
                detail: format!("not a canonical DeviceDirectory: {e}"),
            }
        })?;
        let identity_key = capsule_core::crypto::keys::HybridVerifyingKey::from_bytes(&user_ik)
            .map_err(|e| FfiError::InvalidArgument {
                detail: format!("user_ik is not a hybrid verifying key: {e}"),
            })?;
        let version = DirectoryClient::new(self.session.clone(), &api_base_url)
            .publish(&directory, &identity_key)
            .await?;
        Ok(version)
    }

    /// Fetch a user's signed device directory, **verified under `pinned_user_ik`** (the bytes
    /// [`FfiWorkspace::user_ik_public`](FfiWorkspace::user_ik_public) returns) before it is
    /// handed back. A document that does not verify is an error, never a return value.
    pub async fn fetch_device_directory(
        &self,
        api_base_url: String,
        user_id: String,
        pinned_user_ik: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let user_id = uuid::Uuid::parse_str(&user_id).map_err(|e| FfiError::InvalidArgument {
            detail: format!("user_id: {user_id:?} is not a UUID ({e})"),
        })?;
        let pinned = capsule_core::crypto::keys::HybridVerifyingKey::from_bytes(&pinned_user_ik)
            .map_err(|e| FfiError::InvalidArgument {
                detail: format!("pinned_user_ik is not a hybrid verifying key: {e}"),
            })?;
        let directory = DirectoryClient::new(self.session.clone(), &api_base_url)
            .fetch(user_id, &pinned)
            .await?;
        capsule_core::cbor::to_canonical_vec(&directory).map_err(|e| FfiError::Directory {
            code: None,
            detail: format!("re-encoding the verified directory failed: {e}"),
        })
    }

    /// Revoke the session server-side and clear the local store (idempotent).
    pub async fn logout(&self) -> Result<(), FfiError> {
        self.session.logout().await?;
        Ok(())
    }
}

impl FfiSession {
    /// The session, sending through the current path generation's pooled connections.
    fn path_session(&self) -> Session {
        let generation = self.path.snapshot().generation;
        match self.pool.client(generation) {
            Ok(http) => self.session.with_http_client(http),
            Err(error) => {
                // Building a client fails only where the platform lacks something the builder
                // needs; the session's own client still sends, on the pool it already has.
                tracing::warn!(%error, generation = generation.0, "ffi: no client for the new path; sending on the session's own");
                self.session.clone()
            }
        }
    }

    /// An upload client on the current path's pooled connections.
    fn upload_client(&self) -> UploadClient {
        let transport = UploadTransport::with_session(
            self.path_session(),
            self.upload_base_url.clone(),
            self.protocol_version.clone(),
        );
        UploadClient::new(transport).with_events(self.events.clone())
    }

    /// Drive one bundle's ladder. The workspace lock is not held: the bundle is owned.
    async fn push_bundle(&self, bundle: &UploadBundle) -> Result<AssetPushReport, PushError> {
        tracing::info!(
            asset_id = %bundle.asset_id,
            amk_version = bundle.amk_version,
            original_bytes = bundle.original.size(),
            "ffi: pushing a sealed asset"
        );
        // Full ordering where the path allows it; the plan stages the push where it holds bulk
        // back, and a path change that closes a running tier cancels and re-plans the push.
        let held = HashSet::<String>::new();
        let pushed = run_under_path(&self.path, UploadPolicy::Full, false, |scheduler| {
            let client = self.upload_client();
            let held = &held;
            async move { crate::push::push_bundle(&client, &scheduler, bundle, held, false).await }
        })
        .await;
        if let Some(signal) = path::link_signal(&pushed) {
            let plan = self.path.record(signal);
            tracing::debug!(?signal, class = ?plan.class, "ffi: the push's outcome fed the adverse detector");
        }
        pushed
    }
}

/// Stream the blob at `hash` from `source` into a fresh file at `path`, synced to disk once
/// its content address has checked out. Leaves cleanup of a failed fetch to the caller.
async fn fetch_to_file<B: crate::fetch::BlobSource>(
    source: &B,
    hash: &str,
    size: u64,
    path: &Path,
    class: ConnectionClass,
    events: &TransferEvents,
) -> Result<(), FfiError> {
    let local = |action: &str, e: std::io::Error| FfiError::Fetch {
        code: None,
        detail: format!("{action} {}: {e}", path.display()),
    };
    let file = std::fs::File::create(path).map_err(|e| local("creating", e))?;
    let mut sink = std::io::BufWriter::new(file);
    RangedFetcher::new(class)
        .with_events(events.clone())
        .fetch_into(source, hash, size, &mut sink)
        .await?;
    sink.into_inner()
        .map_err(|e| local("flushing", e.into_error()))?
        .sync_all()
        .map_err(|e| local("syncing", e))
}

#[cfg(test)]
mod tests;
