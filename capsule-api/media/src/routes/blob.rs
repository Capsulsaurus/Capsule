//! Key-free ranged blob serving — `GET /blob/{hash}` (slice `S-C10`; SSoT: the
//! [Download & Sync] blob-fetch surface and [Filesystem — Server]).
//!
//! Serves an opaque ciphertext blob by its **content address**, with HTTP `Range` at the
//! 65,536-byte ciphertext stride (core's `stream::CIPHERTEXT_CHUNK`) so a client resumes and
//! reads mid-file chunks (each chunk decrypts in isolation under core's `decrypt_chunk`). The
//! server holds no key: it serves ciphertext octets, never plaintext,
//! and this endpoint replaces the plaintext-era assumptions in the `LEGACY-PLAINTEXT` asset
//! routes (their retirement is owned by S-G1/S-G3).
//!
//! **Auth per route.** A valid session access token is required (bearer), validated exactly
//! as the storage-verify surface does; a missing/invalid token is `401`.
//!
//! **Status taxonomy** (the load-bearing contract — see [`ServeResolution`]):
//! `200`/`206` served · `404` unknown content address · `410 Gone` taken-down / mid-GC /
//! dangling · `409 error.blob.pending_upload` for a not-yet-uploaded original
//! (`awaiting-original`, transient, **never** `410`). A client switches on the `error.*`
//! code, never the transport status alone ([API Surfaces]).
//!
//! [Download & Sync]: ../../../../../capsule-docs/src/content/docs/design/import/download-sync.md
//! [Filesystem — Server]: ../../../../../capsule-docs/src/content/docs/design/filesystem/server.md
//! [API Surfaces]: ../../../../../capsule-docs/src/content/docs/design/api-surfaces.md

use auth::utils::headers::validate_user_from_headers;
use capsule_i18n::error_codes;
use model::errors::InternalServerError;
use salvo::fs::NamedFile;
use salvo::http::mime;
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use serde::Serialize;

use crate::service::serve::ServeResolution;
use crate::state::AppState;

/// A structured error body carrying the stable `error.*` code clients localize (the English
/// detail message stays English, per the i18n contract).
#[derive(Serialize)]
struct ErrorResponse {
    /// The stable `error.*` code.
    code: &'static str,
    /// The English detail message.
    error: String,
}

/// The `GET /blob/{hash}` response — one arm per [`ServeResolution`] plus the auth/error rails.
pub(super) enum BlobServeResponse {
    /// Present ∧ indexed ∧ retrievable — the ranged file writer serves the ciphertext (`200`
    /// full, `206` partial, `416` unsatisfiable range).
    Serve(Box<NamedFile>),
    /// Unknown / malformed content address — `404`, bodyless (no blob-existence oracle).
    NotFound,
    /// Referenced but gone per policy (quarantined / mid-GC / dangling) — `410 Gone`,
    /// permanent; the client degrades to a lower representation.
    Gone,
    /// The original is not yet uploaded (`awaiting-original`) — `409` + the transient
    /// `error.blob.pending_upload` code, explicitly **distinct from `410`**.
    PendingUpload,
    /// Missing / invalid bearer token — `401`.
    Unauthorized(String),
    /// A server-side failure resolving the blob.
    Internal(InternalServerError),
}

#[async_trait]
impl Writer for BlobServeResponse {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            // NamedFile honors the request's `Range` header: 200 full, 206 partial (with
            // `Content-Range`), 416 unsatisfiable, plus `Accept-Ranges: bytes`.
            Self::Serve(file) => file.write(req, depot, res).await,
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::Gone => {
                res.status_code(StatusCode::GONE);
            }
            Self::PendingUpload => {
                res.status_code(StatusCode::CONFLICT);
                res.render(Json(ErrorResponse {
                    code: error_codes::BLOB_PENDING_UPLOAD,
                    error: "the asset's original has not been uploaded yet".to_string(),
                }));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::Internal(e) => {
                e.write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for BlobServeResponse {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Opaque ciphertext blob (full, or 206 for a range)"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Missing or invalid bearer token"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Unknown content address"),
        );
        operation.responses.insert(
            String::from("409"),
            salvo::oapi::Response::new("Original not yet uploaded (error.blob.pending_upload)"),
        );
        operation.responses.insert(
            String::from("410"),
            salvo::oapi::Response::new("Blob gone (taken down, mid-GC, or dangling)"),
        );
        operation.responses.insert(
            String::from("416"),
            salvo::oapi::Response::new("Requested range not satisfiable"),
        );
    }
}

/// Serve a ciphertext blob by its content address, ranged at the ciphertext stride.
#[endpoint(operation_id = "get_blob", tags("media"), security(("bearer" = [])))]
pub async fn get_blob(
    req: &mut Request,
    depot: &mut Depot,
    hash: PathParam<String>,
) -> BlobServeResponse {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    // Auth per route: a valid session access token is required to fetch any ciphertext.
    if let Err(e) = validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key)
    {
        return BlobServeResponse::Unauthorized(e.to_string());
    }

    let hash = hash.into_inner();
    let resolution = match state.serve.resolve(&state.conn, &hash).await {
        Ok(r) => r,
        Err(e) => return BlobServeResponse::Internal(e.into()),
    };

    match resolution {
        ServeResolution::Serve { path } => match NamedFile::builder(path)
            .content_type(mime::APPLICATION_OCTET_STREAM)
            .build()
            .await
        {
            Ok(mut file) => {
                // An opaque blob fetch, not a named download — no attachment disposition.
                file.disable_content_disposition();
                BlobServeResponse::Serve(Box::new(file))
            }
            // Raced with a delete/GC between the resolve and the open — the blob is no longer
            // retrievable; report it gone rather than a distinguishable 500.
            Err(e) => {
                tracing::warn!(%hash, "blob vanished between resolve and open: {e}");
                BlobServeResponse::Gone
            }
        },
        ServeResolution::NotFound => BlobServeResponse::NotFound,
        ServeResolution::Gone => BlobServeResponse::Gone,
        ServeResolution::PendingUpload => BlobServeResponse::PendingUpload,
    }
}
