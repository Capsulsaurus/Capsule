//! The key-free sync feed service (slice `S-C2`).
//!
//! `Sync(cursor, page_size)` returns a monotonic, resumable page of album changes after an
//! opaque server-MAC'd cursor. Each entry carries the signed manifest as opaque canonical
//! CBOR, the small encrypted metadata blob, per-role blob content addresses, and the derived
//! `original_held` completeness fact — never blob bytes (those stay on the REST `/blob`
//! surface). SSoT: [Download & Sync](https://docs/design/import/download-sync/).
//!
//! Negotiation and rejection ride call metadata per the
//! [api-surfaces](https://docs/design/api-surfaces/) mapping: `x-capsule-protocol` gates the
//! request, `authorization` carries the bearer token, and every rejection carries its stable
//! `error.*` code in the `x-capsule-error-code` trailing metadata.

use auth::claims::Claims;
use capsule_core::validation::{HandshakeReject, protocol_gate};
use capsule_i18n::error_codes;
use entity::sync_entry;
use jsonwebtoken::DecodingKey;
use sea_orm::DatabaseConnection;
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, Query};
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::{Code, Request, Response, Status};

use crate::config::SyncServerConfig;
use crate::cursor::CursorCodec;
use crate::proto::capsule::sync::v1::sync_service_server::SyncService;
use crate::proto::capsule::sync::v1::{
    BlobManifest, BlobRef, ChangeKind as ProtoChangeKind, SyncEntry, SyncRequest, SyncResponse,
};

/// Universal request/response metadata keys (lowercased REST headers, api-surfaces doc).
const MD_PROTOCOL: &str = "x-capsule-protocol";
const MD_PROTOCOL_MIN: &str = "x-capsule-protocol-min";
const MD_PROTOCOL_MAX: &str = "x-capsule-protocol-max";
const MD_ERROR_CODE: &str = "x-capsule-error-code";
const MD_AUTHORIZATION: &str = "authorization";

/// The key-free sync feed service. Entries carry the signed manifest as opaque canonical
/// CBOR plus the encrypted metadata blob and per-role blob references; blob bytes stay on
/// REST.
#[derive(Clone)]
pub struct SyncFeedService {
    db: DatabaseConnection,
    cursor: CursorCodec,
    decoding_key: DecodingKey,
    protocol_min: String,
    protocol_max: String,
    default_page_size: u32,
    max_page_size: u32,
}

impl SyncFeedService {
    /// Build the service from a DB connection and the sync server config.
    #[must_use]
    pub fn new(db: DatabaseConnection, config: &SyncServerConfig) -> Self {
        Self {
            db,
            cursor: CursorCodec::new(&config.cursor_mac_key),
            decoding_key: config.jwt_eddsa_decoding_key.clone(),
            protocol_min: config.protocol_min.clone(),
            protocol_max: config.protocol_max.clone(),
            default_page_size: config.default_page_size,
            max_page_size: config.max_page_size,
        }
    }

    /// Build a rejection `Status` carrying the stable `error.*` code and the accepted
    /// protocol range (advertised on every response, errors included).
    fn reject(&self, code: Code, error_code: &'static str, message: impl Into<String>) -> Status {
        let mut status = Status::new(code, message.into());
        self.advertise(status.metadata_mut());
        status
            .metadata_mut()
            .insert(MD_ERROR_CODE, MetadataValue::from_static(error_code));
        status
    }

    /// Advertise the accepted protocol window on outgoing metadata.
    fn advertise(&self, md: &mut MetadataMap) {
        if let Ok(v) = self.protocol_min.parse() {
            md.insert(MD_PROTOCOL_MIN, v);
        }
        if let Ok(v) = self.protocol_max.parse() {
            md.insert(MD_PROTOCOL_MAX, v);
        }
    }

    /// The universal protocol handshake (invariant 1 / forward-version rejection). A version
    /// above the accepted window is `FAILED_PRECONDITION` (the `426` mapping); a malformed or
    /// missing version is `INVALID_ARGUMENT`. Both carry `error.protocol.version_unsupported`.
    fn negotiate(&self, md: &MetadataMap) -> Result<(), Status> {
        let client = md.get(MD_PROTOCOL).and_then(|v| v.to_str().ok());
        match client {
            Some(version) => match protocol_gate(version, &self.protocol_min, &self.protocol_max) {
                Ok(()) => Ok(()),
                Err(HandshakeReject::ProtocolOutOfRange) => Err(self.reject(
                    Code::FailedPrecondition,
                    error_codes::PROTOCOL_VERSION_UNSUPPORTED,
                    format!(
                        "protocol {version} outside accepted window [{}, {}]",
                        self.protocol_min, self.protocol_max
                    ),
                )),
                Err(_) => Err(self.reject(
                    Code::InvalidArgument,
                    error_codes::PROTOCOL_VERSION_UNSUPPORTED,
                    "x-capsule-protocol is not a YYYY-MM-DD date",
                )),
            },
            None => Err(self.reject(
                Code::FailedPrecondition,
                error_codes::PROTOCOL_VERSION_UNSUPPORTED,
                "missing x-capsule-protocol metadata",
            )),
        }
    }

    /// Authenticate the caller from the `authorization` bearer token, returning the user id.
    fn authenticate(&self, md: &MetadataMap) -> Result<String, Status> {
        let token = md
            .get(MD_AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .ok_or_else(|| {
                self.reject(
                    Code::Unauthenticated,
                    error_codes::SYNC_UNAUTHENTICATED,
                    "missing bearer access token",
                )
            })?;

        let data = Claims::decode(token, &self.decoding_key).map_err(|_| {
            self.reject(
                Code::Unauthenticated,
                error_codes::SYNC_UNAUTHENTICATED,
                "invalid access token",
            )
        })?;
        data.claims.validate_access_token().map_err(|_| {
            self.reject(
                Code::Unauthenticated,
                error_codes::SYNC_UNAUTHENTICATED,
                "access token rejected",
            )
        })?;
        Ok(data.claims.sub)
    }

    /// An internal error that hides its detail from the client.
    fn internal(&self, context: &str, err: impl std::fmt::Display) -> Status {
        tracing::error!(error = %err, "sync feed internal error: {context}");
        Status::internal("internal error")
    }
}

#[tonic::async_trait]
impl SyncService for SyncFeedService {
    #[tracing::instrument(skip(self, request), fields(user_id, after, entries))]
    async fn sync(&self, request: Request<SyncRequest>) -> Result<Response<SyncResponse>, Status> {
        // 1. Forward-version rejection via the universal handshake.
        self.negotiate(request.metadata())?;

        // 2. Authenticate and scope to the caller.
        let user_id = self.authenticate(request.metadata())?;
        tracing::Span::current().record("user_id", tracing::field::display(&user_id));

        // 3. Cursor authenticity (invariant 22): a tampered/forged cursor is rejected here.
        let req = request.get_ref();
        let after = self.cursor.decode(&req.cursor).map_err(|e| {
            tracing::info!(reason = %e, "sync cursor rejected");
            self.reject(
                Code::InvalidArgument,
                error_codes::SYNC_CURSOR_INVALID,
                "sync cursor failed authentication",
            )
        })?;
        tracing::Span::current().record("after", after);

        // 4. Clamp the requested page size.
        let limit = if req.page_size == 0 {
            self.default_page_size
        } else {
            req.page_size.min(self.max_page_size)
        };

        // 5. Scope to the albums the caller may read, then read one forward-only page.
        let album_ids = Query::accessible_album_ids(&self.db, &user_id)
            .await
            .map_err(|e| self.internal("accessible_album_ids", e))?;
        let rows = Query::feed_page(&self.db, &album_ids, after, u64::from(limit))
            .await
            .map_err(|e| self.internal("feed_page", e))?;

        // 6. Map rows to the wire feed; the cursor never regresses (forward-only pagination).
        let next = rows.last().map_or(after, |r| r.feed_seq);
        let entries = rows
            .iter()
            .map(|row| map_entry(row).map_err(|e| self.internal("map_entry", e)))
            .collect::<Result<Vec<_>, _>>()?;
        tracing::Span::current().record("entries", entries.len());

        let next_cursor = self.cursor.encode(next);
        let mut response = Response::new(SyncResponse {
            entries,
            next_cursor,
        });
        self.advertise(response.metadata_mut());
        Ok(response)
    }
}

/// Map a persisted feed row to the wire `SyncEntry`. Ids and content addresses ride as their
/// opaque UTF-8 bytes; the manifest CBOR and metadata blob pass through untouched.
fn map_entry(row: &sync_entry::Model) -> Result<SyncEntry, serde_json::Error> {
    let blobs: FeedBlobManifest = serde_json::from_value(row.blobs.clone())?;
    Ok(SyncEntry {
        album_id: row.album_id.clone().into_bytes(),
        sync_seq: row.sync_seq as u64,
        protocol_version: row.protocol_version.clone(),
        kind: map_kind(row.kind) as i32,
        asset_id: row.asset_id.clone().into_bytes(),
        manifest_cbor: row.manifest_cbor.clone(),
        metadata_blob: row.metadata_blob.clone().unwrap_or_default(),
        blobs: Some(BlobManifest {
            original: blobs.original.map(map_ref),
            derivatives: blobs.derivatives.into_iter().map(map_ref).collect(),
        }),
        original_held: row.original_held,
    })
}

fn map_ref(r: FeedBlobRef) -> BlobRef {
    BlobRef {
        ciphertext_hash: r.ciphertext_hash.into_bytes(),
        role: r.role,
        format: r.format,
        size: r.size,
    }
}

fn map_kind(kind: i16) -> ProtoChangeKind {
    match ChangeKind::from_i16(kind) {
        Some(ChangeKind::Created) => ProtoChangeKind::Created,
        Some(ChangeKind::MetadataUpdated) => ProtoChangeKind::MetadataUpdated,
        Some(ChangeKind::Deleted) => ProtoChangeKind::Deleted,
        None => ProtoChangeKind::Unspecified,
    }
}
