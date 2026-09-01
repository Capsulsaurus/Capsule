//! Public share-link serving endpoints (slice `S-C4`; SSoT: the [Share Links design doc]).
//!
//! Three key-free, unauthenticated endpoints serve an issuer-published share link opaquely:
//!
//! - `GET /s/{opaque-id}` — the per-asset served metadata (each asset's sidecar **stripped** on
//!   serve, no opt-out — Security Contract, Privacy strip on serve).
//! - `GET /s/{opaque-id}/blob/{hash}` — a ciphertext blob the share covers; the client decrypts
//!   it with the link-derived key. Only a hash the resolved link actually covers is served.
//! - `GET /s/{opaque-id}/wrapped-secret` — the issuer-published `WrappedScope` (opaque; the
//!   passphrase-wrapped material when passphrase-protected), unwrapped **client-side**.
//!
//! Every endpoint funnels through [`ShareServeService::resolve_serve`], so all three share one
//! posture: the two rate limiters, the fail-closed revocation cache, and the home-server gate. A
//! not-found / revoked / expired link is one **indistinguishable, bodyless `404`** — never `410`.
//! A link this server does not host returns a structured `{ home_server }` pointer (never an HTTP
//! redirect), never content.
//!
//! [Share Links design doc]: ../../../../capsule-docs/src/content/docs/design/share-links.md
//! [`ShareServeService::resolve_serve`]: crate::service::share::ShareServeService::resolve_serve

use auth::models::errors::ApiError;
use capsule_i18n::error_codes;
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use serde::Serialize;
use service::share::{ServeRecord, ShareScopeKind};

use crate::service::share::ServeOutcome;
use crate::share_state::ShareState;

// ─────────────────────────────── Response bodies ─────────────────────────────────

/// The served metadata for a share link. `assets` carries each covered asset's content address
/// plus the **stripped** metadata blob (base64 canonical CBOR of the export-stripped sidecar).
#[derive(Debug, Serialize)]
struct ShareMetadataResponse {
    /// `"album"` or `"asset"` — the scope the link grants.
    scope: &'static str,
    /// The scoped album/asset id.
    scope_id: String,
    /// The authoritative home server (always this server for a served response).
    home_server: String,
    /// Whether an Argon2id passphrase layer protects the material (the client then fetches the
    /// wrapped secret and unwraps locally).
    passphrase_protected: bool,
    /// RFC 3339 expiry, if any.
    expires_at: Option<String>,
    /// The covered assets.
    assets: Vec<ShareAssetMetadata>,
}

/// One covered asset's served metadata.
#[derive(Debug, Serialize)]
struct ShareAssetMetadata {
    /// The asset id.
    asset_id: String,
    /// The ciphertext blob's content address (fetch it from `/s/{opaque-id}/blob/{hash}`).
    content_hash: String,
    /// The asset's content type.
    content_type: String,
    /// The ciphertext blob size in bytes.
    size: u64,
    /// The **stripped** metadata blob (base64 canonical CBOR); fingerprinting fields removed.
    metadata_blob: String,
    /// The asset's STREAM nonce prefix (lowercase hex) — a key-free crypto-envelope fact the
    /// guest client feeds to `ShareScope.decryptBlob` to decrypt the fetched ciphertext blob. It
    /// carries no meaning without the link secret, so serving it is sanctioned by the Share Links
    /// contract ("the client decrypts using the link-derived key").
    nonce_prefix: String,
    /// The asset's album-master-key epoch (crypto-manifest `amk_version`), the other decrypt
    /// parameter; `0` for an asset-scoped grant whose file key travels in the scope directly.
    amk_version: u32,
}

/// The opaque wrapped-secret payload — the issuer-published `WrappedScope`, unwrapped client-side.
#[derive(Debug, Serialize)]
struct WrappedSecretResponse {
    /// Whether an Argon2id passphrase layer protects the material.
    passphrase_protected: bool,
    /// The `WrappedScope` (base64 canonical CBOR), served opaquely; the server never opens it.
    wrapped_scope: String,
}

/// The structured home-server pointer a non-home peer returns instead of content (never an HTTP
/// redirect — that would be an open-redirect surface).
#[derive(Debug, Serialize)]
struct HomeServerPointer {
    /// The album's authoritative home server; the client re-issues the request there.
    home_server: String,
}

// ─────────────────────────────── Rejections ─────────────────────────────────────

/// A serve-path rejection. `NotFound` renders a **bodyless** `404` (byte-identical across
/// unknown / revoked / expired — the indistinguishable probe). `RateLimited` is a coded `429`.
/// `Foreign` is the `{ home_server }` pointer (`421`), the one distinguishable non-content result.
enum ServeReject {
    /// Not found / revoked / expired / fail-closed — one indistinguishable `404`.
    NotFound,
    /// A per-IP or per-`{opaque-id}` rate limit engaged.
    RateLimited,
    /// This server does not host the share; the client resolves the pointer.
    Foreign {
        /// The authoritative home server.
        home_server: String,
    },
}

#[async_trait]
impl Writer for ServeReject {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            // A bare, bodyless 404: identical bytes and headers for unknown/revoked/expired, so a
            // probe cannot tell them apart (Security Contract — indistinguishable 404).
            ServeReject::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            ServeReject::RateLimited => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.render(Json(ApiError::with_code(
                    "too many share requests",
                    error_codes::SHARE_RATE_LIMITED,
                )));
            }
            // 421 Misdirected Request: this server cannot produce a response for the share; the
            // body carries the home-server pointer the client re-issues against.
            ServeReject::Foreign { home_server } => {
                res.status_code(StatusCode::MISDIRECTED_REQUEST);
                res.render(Json(HomeServerPointer { home_server }));
            }
        }
    }
}

/// The ciphertext blob response (opaque bytes; the client decrypts with the link-derived key).
struct BlobResponse(Vec<u8>);

#[async_trait]
impl Writer for BlobResponse {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        res.status_code(StatusCode::OK);
        // The stored blob is ciphertext — opaque octets regardless of the plaintext content type.
        res.add_header("Content-Type", "application/octet-stream", true)
            .ok();
        if let Err(e) = res.write_body(self.0) {
            tracing::error!("failed to write share blob body: {e}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
}

// ─────────────────────────────── Endpoints ──────────────────────────────────────

/// Serve a share link's per-asset metadata (with the mandatory export strip applied).
#[handler]
pub async fn get_share_metadata(
    req: &mut Request,
    depot: &mut Depot,
    opaque_id: PathParam<String>,
) -> Result<Json<ShareMetadataResponse>, ServeReject> {
    let state = depot
        .obtain::<ShareState>()
        .expect("ShareState injected")
        .clone();
    let record = resolve(&state, req, &opaque_id.into_inner()).await?;

    let assets = state
        .serve
        .stripped_metadata(&record)
        .into_iter()
        .map(|a| ShareAssetMetadata {
            asset_id: a.asset_id,
            content_hash: a.content_hash,
            content_type: a.content_type,
            size: a.size,
            metadata_blob: a.metadata_blob_b64,
            nonce_prefix: a.nonce_prefix_hex,
            amk_version: a.amk_version,
        })
        .collect();

    Ok(Json(ShareMetadataResponse {
        scope: scope_str(&record.scope_kind),
        scope_id: record.scope_id.clone(),
        home_server: record.home_server.clone(),
        passphrase_protected: record.passphrase_protected,
        expires_at: record.expires_at.clone(),
        assets,
    }))
}

/// Serve a ciphertext blob the share covers. Only a content address the resolved link actually
/// covers is served — never an arbitrary blob oracle.
#[handler]
pub async fn get_share_blob(
    req: &mut Request,
    depot: &mut Depot,
    opaque_id: PathParam<String>,
    hash: PathParam<String>,
) -> Result<BlobResponse, ServeReject> {
    let state = depot
        .obtain::<ShareState>()
        .expect("ShareState injected")
        .clone();
    let record = resolve(&state, req, &opaque_id.into_inner()).await?;

    // The hash must be one the share covers (else an indistinguishable 404 — no blob oracle).
    let asset = state
        .serve
        .asset_for_hash(&record, &hash.into_inner())
        .ok_or(ServeReject::NotFound)?;

    // A covered-but-missing blob (server inconsistency) is a 404, not a distinguishable 500.
    let bytes = state
        .storage
        .read_committed_blob(&asset.content_hash)
        .await
        .map_err(|_| ServeReject::NotFound)?;
    Ok(BlobResponse(bytes))
}

/// Serve the opaque wrapped-secret material — the issuer-published `WrappedScope`, unwrapped
/// **client-side** (the passphrase is never transmitted). Rate-limited by the same limiters as
/// the serve path so a failed client-side unwrap cannot be probed server-side.
#[handler]
pub async fn get_wrapped_secret(
    req: &mut Request,
    depot: &mut Depot,
    opaque_id: PathParam<String>,
) -> Result<Json<WrappedSecretResponse>, ServeReject> {
    let state = depot
        .obtain::<ShareState>()
        .expect("ShareState injected")
        .clone();
    let record = resolve(&state, req, &opaque_id.into_inner()).await?;
    Ok(Json(WrappedSecretResponse {
        passphrase_protected: record.passphrase_protected,
        wrapped_scope: record.wrapped_scope_b64.clone(),
    }))
}

// ─────────────────────────────── Helpers ────────────────────────────────────────

/// Resolve an opaque id through the serve engine (rate limits + fail-closed revocation cache +
/// home-server gate), mapping the outcome to a served record or the appropriate rejection.
async fn resolve(
    state: &ShareState,
    req: &Request,
    opaque_id: &str,
) -> Result<Box<ServeRecord>, ServeReject> {
    match state.serve.resolve_serve(opaque_id, &client_ip(req)).await {
        ServeOutcome::Serve(record) => Ok(record),
        ServeOutcome::Foreign { home_server } => Err(ServeReject::Foreign { home_server }),
        ServeOutcome::NotFound => Err(ServeReject::NotFound),
        ServeOutcome::RateLimited => Err(ServeReject::RateLimited),
    }
}

/// A best-effort source-IP string for the per-IP rate limiter (mirrors the drop serve path).
fn client_ip(req: &Request) -> String {
    req.header::<String>("X-Forwarded-For")
        .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
        .or_else(|| req.header::<String>("X-Real-IP"))
        .unwrap_or_else(|| format!("{:?}", req.remote_addr()))
}

/// The wire scope string for a resolved link.
fn scope_str(kind: &ShareScopeKind) -> &'static str {
    match kind {
        ShareScopeKind::Album => "album",
        ShareScopeKind::Asset => "asset",
    }
}
