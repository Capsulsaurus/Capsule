//! uniffi bindings: an async-capable wrapper over the SDK's user-flow primitives
//! (slice `S-D9`), so iOS/macOS (Swift), Android (Kotlin), and Linux consumers call
//! one SDK instead of reimplementing login/upload/sync flows.
//!
//! # Surface
//!
//! [`FfiCapsuleClient`] is the unauthenticated entry point (auth + upload endpoints
//! and the protocol pin). [`FfiCapsuleClient::login`]/[`register`](FfiCapsuleClient::register)
//! return an [`FfiSession`] **handle** — an opaque object that owns the token store
//! internally. Every subsequent call ([`upload`](FfiSession::upload),
//! [`upload_status`](FfiSession::upload_status), [`sync_pull`](FfiSession::sync_pull),
//! [`logout`](FfiSession::logout)) rides that handle. **Tokens never cross the FFI
//! boundary raw**: the S-D7 session store injects and auto-refreshes the bearer
//! behind the handle, so foreign code never sees access/refresh token material.
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
//! `code` (which clients localize) plus the English detail `message` (which stays
//! English) — mirroring the server's `{ error, code }` contract.

use std::sync::Arc;

use capsule_core::lifecycle::LifecycleError;
use secrecy::{ExposeSecret as _, SecretString};

use crate::auth::{AuthClient, AuthError, LoginOutcome, Session};
use crate::directory::{DirectoryClient, DirectoryError};
use crate::recovery::{RecoveryClient, RecoveryError};
use crate::sync::{
    BlobManifest, BlobRef, ChangeKind, FeedEntry, SyncConsumer, SyncCursor, SyncError, SyncPage,
};
use crate::upload::{
    BlobRole, CreateUploadRequest, HeadInfo, ManifestEnvelope, UploadClient, UploadError,
    UploadOutcome, UploadTransport,
};

/// The `capsule-core`-facing workspace verbs (`S-P1`): enroll/open, albums, seal + import,
/// verify, sync-apply, escrow minting, and the signed device directory. Separated from the
/// networked client/session surface below because it is the half that touches no transport.
mod workspace;

pub use workspace::{
    FfiAlbum, FfiAssetFacts, FfiAssetMetadata, FfiClientBuild, FfiDeviceTier, FfiHardwareSigner,
    FfiHardwareSignerError, FfiSyncApplyOutcome, FfiSyncEntry, FfiUploadBlob, FfiVerifyOutcome,
    FfiWorkspace,
};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Every failure the FFI flows can surface, flattened by originating layer. Each
/// variant carries the stable `error.*` catalog `code` (when one applies — clients
/// localize it) and the English detail `message` (stays English), mirroring the
/// SDK's `{ error, code }` contract so foreign apps switch on the code, never a
/// bare HTTP status.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// An authentication flow (login/register/refresh/logout) failed.
    #[error("authentication failed ({code:?}): {message}")]
    Auth {
        /// Stable `error.*` catalog code, when the SDK mapped one.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
    },
    /// An upload flow (create/transfer/status) failed.
    #[error("upload failed ({code:?}): {message}")]
    Upload {
        /// Stable `error.*` catalog code, when the server supplied one.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
    },
    /// A sync flow (pull/reconcile) failed.
    #[error("sync failed ({code:?}): {message}")]
    Sync {
        /// Stable `error.*` catalog code, when one applies.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
    },
    /// A local workspace operation failed — opening the library, sealing an asset, reading a
    /// managed asset. This is the *workspace* failing, never an entry being refused: a refused
    /// sync entry is a [`FfiSyncApplyOutcome::Quarantined`] verdict, not an error.
    #[error("workspace error: {message}")]
    Workspace {
        /// English detail (developer/log message).
        message: String,
    },
    /// A master-key escrow flow (store/fetch) failed.
    #[error("escrow failed ({code:?}): {message}")]
    Escrow {
        /// Stable `error.*` catalog code, when the server supplied one — `error.escrow.*`
        /// separates "you have no recovery backup" (a setup prompt) from "we could not read
        /// it" (a retry), which is the whole reason the catalog distinguishes them.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
    },
    /// A device-directory flow (publish/fetch) failed.
    #[error("device directory failed ({code:?}): {message}")]
    Directory {
        /// Stable `error.*` catalog code, when one applies.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
    },
    /// A foreign argument was structurally invalid (e.g. an un-parseable URL) before
    /// any network call.
    #[error("invalid argument: {message}")]
    InvalidArgument {
        /// What was wrong with the argument.
        message: String,
    },
}

impl From<LifecycleError> for FfiError {
    fn from(err: LifecycleError) -> Self {
        Self::Workspace {
            message: err.to_string(),
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
            return Self::Auth {
                code,
                message: detail,
            };
        }
        // A malformed argument is the caller's, not the escrow surface's.
        if let RecoveryError::InvalidBaseUrl { .. } = err {
            return Self::InvalidArgument {
                message: err.to_string(),
            };
        }
        Self::Escrow {
            code: err.error_code().map(str::to_owned),
            message: err.to_string(),
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
            message: err.to_string(),
        }
    }
}

impl From<AuthError> for FfiError {
    fn from(err: AuthError) -> Self {
        let code = err.error_code().map(str::to_string);
        match &err {
            AuthError::InvalidBaseUrl { .. } => Self::InvalidArgument {
                message: err.to_string(),
            },
            _ => Self::Auth {
                code,
                message: err.to_string(),
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
            UploadError::Rejected { code, .. } => code.clone(),
            _ => None,
        };
        Self::Upload {
            code,
            message: err.to_string(),
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
            message: err.to_string(),
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
}

impl From<FfiBlobRole> for BlobRole {
    fn from(role: FfiBlobRole) -> Self {
        match role {
            FfiBlobRole::Original => Self::Original,
            FfiBlobRole::Derivative => Self::Derivative,
            FfiBlobRole::Metadata => Self::Metadata,
            FfiBlobRole::Provenance => Self::Provenance,
            FfiBlobRole::Backup => Self::Backup,
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
    /// - `cohort_hash` is the optional advisory device-cohort digest (S-D11); `None`
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

/// What a password login answered with, across the FFI (`S-C63`).
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
            // The password verified and the sign-in is not finished (`S-C55`). No session
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

    /// Complete a sign-in with the code an authenticator app is showing (`S-C55`).
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
    /// An address and a password, and nothing else: the server takes nothing else (`S-C53`).
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
        })
    }
}

// ─── Session (authenticated handle) ──────────────────────────────────────────

/// An authenticated session handle. Owns the S-D7 token store internally (auto
/// pre-flight refresh, single-flight, `401`-retry-once); **no token ever crosses
/// the FFI boundary**. Every flow rides this handle.
#[derive(uniffi::Object)]
pub struct FfiSession {
    session: Session,
    upload_base_url: String,
    protocol_version: String,
}

#[uniffi::export(async_runtime = "tokio")]
impl FfiSession {
    /// Whether the session currently holds tokens (`false` after logout).
    pub async fn is_authenticated(&self) -> bool {
        self.session.is_authenticated().await
    }

    /// Upload a full ciphertext blob (`data`) under `request`, driving the adaptive,
    /// resumable protocol with the code-driven recovery matrix.
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

    /// Pull one page of the key-free sync feed after `cursor` from `api_base_url`
    /// (`http[s]://host:port`). Pass an empty `cursor` for the first page; persist and pass
    /// back `next_cursor` to advance. The bearer rides the `Authorization` header
    /// (auto-refreshed); it never crosses the FFI boundary.
    ///
    /// The parameter is the **API base URL** now, not a gRPC endpoint: the feed is
    /// `GET /v1/sync` on the same origin as every other call (`S-D28`).
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
    pub async fn escrow_put(&self, api_base_url: String, blob: Vec<u8>) -> Result<(), FfiError> {
        let blob =
            capsule_core::cbor::from_slice(&blob).map_err(|e| FfiError::InvalidArgument {
                message: format!("escrow blob is not a canonical WrappedSecret: {e}"),
            })?;
        RecoveryClient::new(self.session.clone(), &api_base_url)?
            .store_escrow(&blob)
            .await?;
        Ok(())
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
            message: format!("encoding the fetched escrow blob failed: {e}"),
        })
    }

    /// Publish this device's **signed device directory**, returning the `directory_version` the
    /// server now stores. `directory_cbor` is the opaque document
    /// [`FfiWorkspace::signed_device_directory`](FfiWorkspace::signed_device_directory)
    /// produced; it travels verbatim, because re-encoding it would detach it from the signature
    /// it carries.
    ///
    /// Publish when the document changes (a device enrolled or revoked). The version must
    /// advance — republishing an unchanged document answers the version-conflict code.
    pub async fn publish_device_directory(
        &self,
        api_base_url: String,
        directory_cbor: Vec<u8>,
    ) -> Result<u64, FfiError> {
        let directory = capsule_core::cbor::from_slice(&directory_cbor).map_err(|e| {
            FfiError::InvalidArgument {
                message: format!("not a canonical DeviceDirectory: {e}"),
            }
        })?;
        let version = DirectoryClient::new(self.session.clone(), &api_base_url)
            .publish(&directory)
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
            message: format!("user_id: {user_id:?} is not a UUID ({e})"),
        })?;
        let pinned = capsule_core::crypto::keys::HybridVerifyingKey::from_bytes(&pinned_user_ik)
            .map_err(|e| FfiError::InvalidArgument {
                message: format!("pinned_user_ik is not a hybrid verifying key: {e}"),
            })?;
        let directory = DirectoryClient::new(self.session.clone(), &api_base_url)
            .fetch(user_id, &pinned)
            .await?;
        capsule_core::cbor::to_canonical_vec(&directory).map_err(|e| FfiError::Directory {
            code: None,
            message: format!("re-encoding the verified directory failed: {e}"),
        })
    }

    /// Revoke the session server-side and clear the local store (idempotent).
    pub async fn logout(&self) -> Result<(), FfiError> {
        self.session.logout().await?;
        Ok(())
    }
}

impl FfiSession {
    fn upload_client(&self) -> UploadClient {
        let transport = UploadTransport::with_session(
            self.session.clone(),
            self.upload_base_url.clone(),
            self.protocol_version.clone(),
        );
        UploadClient::new(transport)
    }
}

#[cfg(test)]
mod tests;
