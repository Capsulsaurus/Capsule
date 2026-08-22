//! Storage verification — `POST /storage/verify` (slice `S-C3` in the repo-root
//! `SLICES.md`; SSoT: <https://docs/design/import/storage-verification/>).
//!
//! The key-free durability query: for each asset, confirm every declared blob is
//! **stored** (present in `blobs/` at its content address), **indexed** (a committed
//! `uploaded = true` row references it), and **retrievable** (refcount > 0, not mid-GC,
//! not quarantined). The verdict gates every destructive local cleanup on clients (the
//! verify-before-destroy rule; the client-side predicate is
//! `capsule_core::library::release_is_safe`). The request is a read and writes no state.
//!
//! The signed `StorageAttestation` form of this verdict (`signed: true` + optional `nonce`,
//! slice `S-C15`) wraps the same unsigned engine: the verdict is computed exactly as the
//! unsigned path, then sealed under the server attestation key with the client nonce echoed
//! verbatim (invariant 34). Signing is server-priced like `deep` — rate-limited per user.

use auth::utils::headers::validate_user_from_headers;
use base64::Engine as _;
use capsule_i18n::error_codes;
use model::errors::InternalServerError;
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use service::attestation::{AttestedBlob, AttestedVerdict, StorageAttestation};

use crate::service::verify::{AssetQuery, AssetVerdict, VerifyError};
use crate::state::AppState;

/// A SHA-256 content address is exactly 64 lowercase-hex characters.
const CONTENT_HASH_LEN: usize = 64;

/// One asset to verify: the exact blob hashes the client is relying on.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AssetVerifyRequest {
    /// The asset id.
    pub asset_id: String,
    /// Content addresses (hex) of every blob the client relies on.
    pub blob_hashes: Vec<String>,
}

/// The `POST /storage/verify` request body.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct StorageVerifyRequest {
    /// The assets to verify.
    pub assets: Vec<AssetVerifyRequest>,
    /// Re-read and re-hash blob bytes instead of trusting stat + index (rate-limited and
    /// coalesced server-side).
    #[serde(default)]
    pub deep: bool,
    /// Return a server-signed [`StorageAttestation`] per asset instead of the bare verdict —
    /// evidence a client can retain. Server-priced like `deep` (rate-limited per user).
    #[serde(default)]
    pub signed: bool,
    /// An optional client freshness challenge (base64), echoed verbatim into every signed
    /// attestation so a stale `durable = true` cannot be replayed as current. Ignored unless
    /// `signed`.
    #[serde(default)]
    pub nonce: Option<String>,
}

/// One blob's verdict.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct BlobVerdictResponse {
    /// The declared content address (hex).
    pub hash: String,
    /// `original | metadata | derivative | provenance` — or `unknown` for a hash the server
    /// does not associate with the asset.
    pub role: String,
    /// Present in the blob store at its content address.
    pub stored: bool,
    /// Referenced by a committed, `uploaded = true` row.
    pub indexed: bool,
    /// Refcount > 0, not `collectable_since`, not quarantined.
    pub retrievable: bool,
}

/// One asset's verdict.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct StorageVerdictResponse {
    /// The asset id.
    pub asset_id: String,
    /// All required blobs stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// Per-blob detail, one entry per declared hash (a hash the server does not associate
    /// with the asset comes back `stored=false, indexed=false` — never silently omitted).
    pub blobs: Vec<BlobVerdictResponse>,
    /// The server's trusted clock at verification (RFC 3339).
    pub checked_at: String,
}

/// The `POST /storage/verify` response body.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct StorageVerifyResponse {
    /// One verdict per requested asset.
    pub verdicts: Vec<StorageVerdictResponse>,
}

/// One signed attestation in the `signed: true` response — the decoded verdict plus the
/// canonical-CBOR signed form (base64) the client verifies under the attestation key.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SignedAttestationResponse {
    /// The verdict (identical to the unsigned form).
    pub verdict: StorageVerdictResponse,
    /// This server's canonical origin.
    pub server_id: String,
    /// The attestation key fingerprint that signed (hex).
    pub server_key_id: String,
    /// The client nonce, echoed verbatim (base64), or absent if none was sent.
    pub nonce: Option<String>,
    /// The full signed `StorageAttestation` as canonical CBOR (base64) — verify under the
    /// server's published attestation key.
    pub attestation_cbor: String,
}

/// The `POST /storage/verify` response body when `signed: true`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SignedStorageVerifyResponse {
    /// One signed attestation per requested asset.
    pub attestations: Vec<SignedAttestationResponse>,
}

/// A structured error body carrying the stable `error.*` code clients localize.
#[derive(Serialize)]
struct ErrorResponse {
    code: &'static str,
    error: String,
}

/// Possible responses for storage verification.
pub(super) enum StorageVerifyResponses {
    /// Verdicts computed (a non-durable asset is still a 200 — the verdict carries it).
    Ok(StorageVerifyResponse),
    /// Signed attestations computed (`signed: true`).
    OkSigned(SignedStorageVerifyResponse),
    /// Structurally invalid request (unknown asset id shape, malformed hash).
    BadRequest(String),
    /// Missing / invalid bearer token.
    Unauthorized(String),
    /// The caller exceeded its per-user deep-scan or signed-attestation budget.
    RateLimited,
    /// A server-side failure computing the verdict.
    Internal(InternalServerError),
}

impl From<Vec<AssetVerdict>> for StorageVerifyResponse {
    fn from(verdicts: Vec<AssetVerdict>) -> Self {
        Self {
            verdicts: verdicts
                .into_iter()
                .map(|v| StorageVerdictResponse {
                    asset_id: v.asset_id,
                    durable: v.durable,
                    blobs: v
                        .blobs
                        .into_iter()
                        .map(|b| BlobVerdictResponse {
                            hash: b.hash,
                            role: b.role.as_str().to_string(),
                            stored: b.stored,
                            indexed: b.indexed,
                            retrievable: b.retrievable,
                        })
                        .collect(),
                    checked_at: v.checked_at.to_string(),
                })
                .collect(),
        }
    }
}

capsule_wire::salvo_responses! {
    StorageVerifyResponses {
        Ok(data) => 200, json(data),
            doc("Per-asset durability verdicts", schema = StorageVerifyResponse);
        OkSigned(data) => 200, json(data), undocumented();
        BadRequest(msg) => 400, json(ErrorResponse {
            code: error_codes::STORAGE_INVALID_REQUEST,
            error: msg,
        }), doc("Structurally invalid request");
        Unauthorized(msg) => 401, text(msg), doc("Missing or invalid bearer token");
        RateLimited {} => 429, json(ErrorResponse {
            code: error_codes::STORAGE_DEEP_RATE_LIMITED,
            error: "deep-scan rate limit exceeded".to_string(),
        }), doc("Deep-scan rate limit exceeded");
        Internal(e) => _, delegate(e), undocumented();
    }
}

/// A content address is exactly 64 lowercase-hex characters.
fn is_valid_content_hash(hash: &str) -> bool {
    hash.len() == CONTENT_HASH_LEN
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The parsed, validated verify request.
struct ParsedRequest {
    assets: Vec<AssetQuery>,
    deep: bool,
    signed: bool,
    nonce: Option<Vec<u8>>,
}

/// Structurally validate + convert the wire request into the service query shape.
fn parse_request(body: StorageVerifyRequest) -> Result<ParsedRequest, String> {
    let mut assets = Vec::with_capacity(body.assets.len());
    for asset in body.assets {
        if asset.asset_id.trim().is_empty() {
            return Err("asset_id must be non-empty".to_string());
        }
        for hash in &asset.blob_hashes {
            if !is_valid_content_hash(hash) {
                return Err(format!("malformed content hash: {hash:?}"));
            }
        }
        assets.push(AssetQuery {
            asset_id: asset.asset_id,
            blob_hashes: asset.blob_hashes,
        });
    }
    let nonce = match body.nonce {
        Some(b64) => Some(
            base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .map_err(|e| format!("nonce is not valid base64: {e}"))?,
        ),
        None => None,
    };
    Ok(ParsedRequest {
        assets,
        deep: body.deep,
        signed: body.signed,
        nonce,
    })
}

/// Map an unsigned [`AssetVerdict`] into the signable [`AttestedVerdict`] shape.
fn to_attested(verdict: &AssetVerdict) -> AttestedVerdict {
    AttestedVerdict {
        asset_id: verdict.asset_id.clone(),
        durable: verdict.durable,
        blobs: verdict
            .blobs
            .iter()
            .map(|b| AttestedBlob {
                hash: b.hash.clone(),
                role: b.role.as_str().to_string(),
                stored: b.stored,
                indexed: b.indexed,
                retrievable: b.retrievable,
            })
            .collect(),
        checked_at: verdict.checked_at.to_string(),
    }
}

/// Build the decoded response view for one signed attestation.
fn signed_response(att: &StorageAttestation) -> SignedAttestationResponse {
    let verdict = &att.core.verdict;
    SignedAttestationResponse {
        verdict: StorageVerdictResponse {
            asset_id: verdict.asset_id.clone(),
            durable: verdict.durable,
            blobs: verdict
                .blobs
                .iter()
                .map(|b| BlobVerdictResponse {
                    hash: b.hash.clone(),
                    role: b.role.clone(),
                    stored: b.stored,
                    indexed: b.indexed,
                    retrievable: b.retrievable,
                })
                .collect(),
            checked_at: verdict.checked_at.clone(),
        },
        server_id: att.core.server_id.clone(),
        server_key_id: att.core.server_key_id.to_hex(),
        nonce: att
            .core
            .nonce
            .as_ref()
            .map(|n| base64::engine::general_purpose::STANDARD.encode(n)),
        attestation_cbor: base64::engine::general_purpose::STANDARD
            .encode(capsule_core::cbor::to_canonical_vec(att).unwrap_or_default()),
    }
}

/// Batch-confirm that assets' blobs are stored, indexed, and retrievable on this server.
#[endpoint(operation_id = "storage_verify", tags("storage"), security(("bearer" = [])))]
pub async fn storage_verify(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<StorageVerifyRequest>,
) -> StorageVerifyResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return StorageVerifyResponses::Unauthorized(e.to_string()),
        };

    let ParsedRequest {
        assets,
        deep,
        signed,
        nonce,
    } = match parse_request(body.into_inner()) {
        Ok(parsed) => parsed,
        Err(msg) => return StorageVerifyResponses::BadRequest(msg),
    };

    // The signed path is server-priced exactly like `deep`: charge the per-user budget before
    // producing any signature so a client cannot amplify server CPU.
    if signed && let Err(VerifyError::SignRateLimited) = state.verify.charge_sign(&user_id).await {
        return StorageVerifyResponses::RateLimited;
    }

    let verdicts = match state
        .verify
        .verify(&state.conn, &user_id, &assets, deep)
        .await
    {
        Ok(verdicts) => verdicts,
        Err(VerifyError::DeepRateLimited | VerifyError::SignRateLimited) => {
            return StorageVerifyResponses::RateLimited;
        }
        Err(e @ (VerifyError::Db(_) | VerifyError::Hash(_))) => {
            return StorageVerifyResponses::Internal(InternalServerError::from(e));
        }
    };

    if !signed {
        return StorageVerifyResponses::Ok(verdicts.into());
    }

    // Sign each verdict over the same state read that produced it, echoing the nonce verbatim
    // (invariant 34). The attestation key is the same hybrid keyring the upload server signs
    // receipts with.
    let keyring = &state.config.attestation;
    let attestations = verdicts
        .iter()
        .map(|v| {
            let att = keyring.attest_verdict(to_attested(v), nonce.clone());
            signed_response(&att)
        })
        .collect();
    StorageVerifyResponses::OkSigned(SignedStorageVerifyResponse { attestations })
}
