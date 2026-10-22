//! Verify-before-destroy: the client half of storage verification (slice `S-D4`; SSoT:
//! [Storage Verification — Verify Before Destroy]).
//!
//! Before any post-write local cleanup of irreplaceable bytes — releasing a device-owned
//! original, deleting a Move-import source, a streaming-mode release — a client requires
//! **three** complementary facts, all fetched here and combined through the offline data
//! plane's pure predicates:
//!
//! 1. **Durability.** [`StorageVerifyClient::verify`] calls `POST /storage/verify` and maps the
//!    response into [`capsule_core::library::StorageVerdict`]; the gate applies
//!    [`capsule_core::library::release_is_safe`], which re-checks every declared blob rather
//!    than trusting the server's aggregate.
//! 2. **Accountability.** [`StorageVerifyClient::fetch_receipt`] fetches the write's
//!    [`CustodyReceipt`] and
//!    [`capsule_core::library::verify_receipt`] checks the hybrid signature under the pinned
//!    attestation key plus a field match against what the client sent. A server that withholds
//!    receipts never becomes the sole holder of an only-copy.
//! 3. **Crypto validity.** `verify_asset` (the caller's offline check) must have accepted the
//!    asset — passed in as `verify_asset_accepted`.
//!
//! Beside the gate, [`StorageVerifyClient::verify_signed`] asks for the verdict in its **signed**
//! form, bound to a client nonce and verified under the same pinned attestation keys, so a
//! client can keep the server's own statement of what it holds — or has lost — as evidence.
//!
//! [`ReleaseCoordinator`] composes the three and enforces the **60-second re-verify window**:
//! the verdict is a point-in-time fact, so a verdict older than the window is re-fetched before
//! release. The result is a [`capsule_core::library::ReleaseDecision`]; a non-`Release` outcome
//! never destroys — the caller retains the local copy, retries with backoff, and surfaces
//! "not yet confirmed on server". Streaming import (`S-B3`) drives this per asset in its
//! import→upload→verify→release window.
//!
//! [Storage Verification — Verify Before Destroy]: https://docs/design/transfer/durability/#verify-before-destroy

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::HybridVerifyingKey;
use capsule_core::crypto::storage_attestation::{
    ATTESTATION_NONCE_LEN, AttestationNonce, AttestationRejection, DeclaredAsset,
    StorageAttestation, verify_storage_attestation,
};
use capsule_core::library::{
    BlobRole, BlobVerdict, CustodyReceipt, ReceiptExpectations, ReleaseDecision, RetainReason,
    StorageVerdict, StorageVerifier, VerifierError, release_is_safe, verify_receipt,
};
use capsule_core::lifecycle::UploadBundle;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};
use uuid::Uuid;

// ─── Errors ───────────────────────────────────────────────────────────────────

/// A failure fetching or decoding a verdict, receipt, or attestation-key document. The gate
/// folds these into a `Retain` decision — a client never destroys on an unresolved fetch.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The HTTP request failed on the wire, or the session could not authorize it.
    #[error("storage-verify transport: {0}")]
    Transport(String),
    /// The server answered with a non-success status.
    #[error("storage-verify server status {status}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// The stable `error.*` code, when the server supplied one.
        code: Option<String>,
    },
    /// The response body was missing a field or otherwise unparsable.
    #[error("malformed storage-verify response: {0}")]
    Malformed(String),
    /// A signed verdict came back and is not evidence for this request: a signature that does
    /// not verify under a pinned attestation key, another request's nonce, or verdicts that
    /// answer a different question.
    #[error("storage attestation rejected: {0}")]
    Attestation(AttestationRejection),
    /// The server will not take this client's writes: its `426`, or a window it advertised
    /// that already refuses this build (issue #674). Update the app.
    #[error(transparent)]
    UpgradeRequired(Box<crate::net::negotiation::UpgradeRequired>),
}

impl VerifyError {
    /// The stable `error.*` code, when one applies.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Status { code, .. } => code.as_deref(),
            Self::UpgradeRequired(refusal) => Some(refusal.error_code()),
            _ => None,
        }
    }
}

impl From<crate::auth::AuthError> for VerifyError {
    fn from(err: crate::auth::AuthError) -> Self {
        match err {
            crate::auth::AuthError::UpgradeRequired(refusal) => {
                VerifyError::UpgradeRequired(refusal)
            }
            other => VerifyError::Transport(other.to_string()),
        }
    }
}

impl From<crate::net::negotiation::SendError> for VerifyError {
    fn from(err: crate::net::negotiation::SendError) -> Self {
        match err {
            crate::net::negotiation::SendError::Upgrade(refusal) => {
                VerifyError::UpgradeRequired(Box::new(refusal))
            }
            crate::net::negotiation::SendError::Transport(error) => {
                VerifyError::Transport(error.to_string())
            }
        }
    }
}

// ─── Authorized transport ──────────────────────────────────────────────────────

/// A fixed bearer token, for tests and callers that already hold a live token.
#[derive(Debug, Clone)]
pub struct StaticToken(pub String);

#[derive(Clone)]
enum VerifyAuth {
    /// Drive requests through the `S-D7` session (pre-flight refresh, single-flight, one `401`
    /// refresh-and-replay).
    Session(crate::auth::Session),
    /// A fixed bearer over a plain client (tests).
    Static {
        http: reqwest::Client,
        token: String,
    },
}

/// The authorized HTTP transport for the storage-verification + receipt surfaces: the API root
/// (no trailing slash) plus the authorization seam.
#[derive(Clone)]
pub struct VerifyTransport {
    base_url: String,
    auth: VerifyAuth,
}

impl VerifyTransport {
    /// Build a transport that authorizes through an authenticated `S-D7` session — the
    /// sanctioned production path. `base_url` is the API root (`POST {base}/storage/verify`,
    /// `GET {base}/upload/{id}/receipt`, `GET {base}/assets/{id}/receipts`); the attestation-key
    /// record is resolved against its origin instead (see
    /// [`StorageVerifyClient::fetch_attestation_keys`]).
    pub fn with_session(session: crate::auth::Session, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: VerifyAuth::Session(session),
        }
    }

    /// Build a transport over a fixed bearer token (tests; callers holding a live token).
    ///
    /// `http` **must** come from [`crate::net::http_builder`] or [`crate::net::http_client`]: a
    /// client built any other way sends no protocol handshake, and every gated route refuses it.
    pub fn with_static_token(
        http: reqwest::Client,
        base_url: impl Into<String>,
        token: StaticToken,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: VerifyAuth::Static {
                http,
                token: token.0,
            },
        }
    }

    async fn send<F>(&self, build: F) -> Result<reqwest::Response, VerifyError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        match &self.auth {
            VerifyAuth::Session(session) => Ok(session.execute(build).await?),
            VerifyAuth::Static { http, token } => Ok(crate::net::negotiation::send_negotiated(
                build(http).bearer_auth(token),
            )
            .await?),
        }
    }
}

// ─── Wire DTOs (mirror the server's transport JSON) ─────────────────────────────

/// One asset to verify: the exact blob hashes the client is relying on.
#[derive(Debug, Clone)]
pub struct AssetQuery {
    /// The asset id.
    pub asset_id: Uuid,
    /// Every blob content address (as `Hash32`) the client relies on.
    pub blob_hashes: Vec<Hash32>,
}

#[derive(Serialize)]
struct AssetVerifyRequestWire {
    asset_id: String,
    blob_hashes: Vec<String>,
}

#[derive(Serialize)]
struct StorageVerifyRequestWire {
    assets: Vec<AssetVerifyRequestWire>,
    deep: bool,
    /// Present only on a signed request, so an unsigned one is byte-identical to before.
    #[serde(skip_serializing_if = "Option::is_none")]
    attestation_nonce: Option<String>,
}

#[derive(Deserialize)]
struct BlobVerdictWire {
    hash: String,
    role: String,
    stored: bool,
    indexed: bool,
    retrievable: bool,
}

#[derive(Deserialize)]
struct StorageVerdictWire {
    asset_id: String,
    durable: bool,
    blobs: Vec<BlobVerdictWire>,
    checked_at: String,
}

#[derive(Deserialize)]
struct StorageVerifyResponseWire {
    verdicts: Vec<StorageVerdictWire>,
    /// Base64 of the canonical-CBOR [`StorageAttestation`], on a signed request only.
    #[serde(default)]
    attestation: Option<String>,
}

#[derive(Deserialize)]
struct ApiErrorWire {
    #[serde(default)]
    code: Option<String>,
}

/// The role string the server uses, mapped to the closed core enum (`unknown` and anything
/// unrecognized fall to `Derivative` — the safest bucket, since it does not satisfy the
/// required-blob set the gate re-checks).
fn role_from_str(role: &str) -> BlobRole {
    match role {
        "original" => BlobRole::Original,
        "metadata" => BlobRole::Metadata,
        "provenance" => BlobRole::Provenance,
        _ => BlobRole::Derivative,
    }
}

impl StorageVerdictWire {
    fn into_core(self) -> Result<StorageVerdict, VerifyError> {
        let asset_id = Uuid::parse_str(&self.asset_id)
            .map_err(|e| VerifyError::Malformed(format!("verdict asset_id: {e}")))?;
        let blobs = self
            .blobs
            .into_iter()
            .map(|b| {
                let hash = Hash32::from_hex(&b.hash)
                    .map_err(|_| VerifyError::Malformed(format!("blob hash: {}", b.hash)))?;
                Ok(BlobVerdict {
                    hash,
                    role: role_from_str(&b.role),
                    stored: b.stored,
                    indexed: b.indexed,
                    retrievable: b.retrievable,
                })
            })
            .collect::<Result<Vec<_>, VerifyError>>()?;
        Ok(StorageVerdict {
            asset_id,
            durable: self.durable,
            blobs,
            checked_at: self.checked_at,
        })
    }
}

/// A signed verdict that verified for the request it answers.
///
/// `cbor` is the attestation exactly as the server signed it — the bytes a client keeps as
/// evidence, beside its custody receipts. `verdicts` is read from the **signed** core, never from
/// the unsigned JSON projection beside it, so nothing a caller acts on was outside the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAttestation {
    /// The decoded, verified attestation.
    pub attestation: StorageAttestation,
    /// Its canonical-CBOR bytes, as received.
    pub cbor: Vec<u8>,
    /// The signed verdicts, one per asset asked about, in request order.
    pub verdicts: Vec<StorageVerdict>,
}

/// Read the verdicts out of a verified attestation's signed core.
fn signed_verdicts(attestation: &StorageAttestation) -> Result<Vec<StorageVerdict>, VerifyError> {
    attestation
        .core
        .verdicts
        .iter()
        .map(|asset| {
            let asset_id = Uuid::parse_str(&asset.asset_id)
                .map_err(|e| VerifyError::Malformed(format!("attested asset_id: {e}")))?;
            Ok(StorageVerdict {
                asset_id,
                durable: asset.durable,
                blobs: asset
                    .blobs
                    .iter()
                    .map(|blob| BlobVerdict {
                        hash: blob.hash,
                        role: role_from_str(&blob.role),
                        stored: blob.stored,
                        indexed: blob.indexed,
                        retrievable: blob.retrievable,
                    })
                    .collect(),
                checked_at: attestation.core.checked_at.clone(),
            })
        })
        .collect()
}

/// One entry of the `GET /assets/{id}/receipts` chain: `receipt_cbor` is base64 of the full
/// signed canonical-CBOR receipt (the route's other fields are a reading of those bytes and are
/// not trusted). `GET /upload/{id}/receipt` does **not** use this shape — it serves the raw
/// `application/cbor` bytes; see [`StorageVerifyClient::fetch_receipt`].
#[derive(Deserialize)]
struct ReceiptWire {
    receipt_cbor: String,
}

#[derive(Deserialize)]
struct AssetReceiptsResponseWire {
    receipts: Vec<ReceiptWire>,
}

/// The `.well-known` attestation-key publication (append-only history).
#[derive(Deserialize)]
struct PublishedKeyWire {
    public: String,
}

#[derive(Deserialize)]
struct WellKnownAttestationWire {
    keys: Vec<PublishedKeyWire>,
}

// ─── The client ─────────────────────────────────────────────────────────────────

/// The storage-verification + custody-receipt client over an authorized [`VerifyTransport`].
#[derive(Clone)]
pub struct StorageVerifyClient {
    transport: VerifyTransport,
}

impl StorageVerifyClient {
    /// Build a client over an authorized transport.
    pub fn new(transport: VerifyTransport) -> Self {
        Self { transport }
    }

    /// `POST /storage/verify`: confirm each asset's declared blobs are stored, indexed, and
    /// retrievable. `deep` opt-in re-hashes bytes (server-priced) — leave `false` on the hot
    /// verify-before-destroy path.
    #[instrument(skip_all, fields(assets = assets.len(), deep))]
    pub async fn verify(
        &self,
        assets: &[AssetQuery],
        deep: bool,
    ) -> Result<Vec<StorageVerdict>, VerifyError> {
        let wire = self.post_verify(assets, deep, None).await?;
        wire.verdicts
            .into_iter()
            .map(StorageVerdictWire::into_core)
            .collect()
    }

    /// `POST /storage/verify` for the **signed** verdict, bound to `nonce`, verified under
    /// `pinned_keys` (the published attestation-key history).
    ///
    /// Returns the attestation only when its signature verifies, it carries `nonce`, and its
    /// verdicts answer exactly `assets` — the same assets and declared hashes, in order. Anything
    /// else is [`VerifyError::Attestation`]; a response with no attestation at all is
    /// [`VerifyError::Malformed`]. The server prices a signed request like `deep`, so a spent
    /// budget is [`VerifyError::Status`] with `429` and `error.storage.deep_rate_limited`.
    ///
    /// `nonce` must be fresh per request — [`ReleaseCoordinator::fetch_verified_attestation`]
    /// draws one — or a replayed answer to an earlier request would verify.
    #[instrument(skip_all, fields(assets = assets.len(), deep))]
    pub async fn verify_signed(
        &self,
        assets: &[AssetQuery],
        deep: bool,
        nonce: &AttestationNonce,
        pinned_keys: &[HybridVerifyingKey],
    ) -> Result<VerifiedAttestation, VerifyError> {
        let wire = self.post_verify(assets, deep, Some(nonce)).await?;
        let encoded = wire.attestation.ok_or_else(|| {
            VerifyError::Malformed("a signed verdict was asked for and none came back".into())
        })?;
        let cbor = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| VerifyError::Malformed(format!("attestation base64: {e}")))?;
        let attestation = StorageAttestation::from_canonical_cbor(&cbor)
            .map_err(|e| VerifyError::Malformed(format!("attestation cbor: {e}")))?;

        let declared: Vec<DeclaredAsset> = assets
            .iter()
            .map(|asset| DeclaredAsset {
                asset_id: asset.asset_id.to_string(),
                blob_hashes: asset.blob_hashes.clone(),
            })
            .collect();
        verify_storage_attestation(&attestation, pinned_keys, nonce, &declared).map_err(
            |rejection| {
                warn!(%rejection, "a storage attestation did not verify");
                VerifyError::Attestation(rejection)
            },
        )?;
        let verdicts = signed_verdicts(&attestation)?;
        debug!(
            key_id = %attestation.core.server_key_id,
            checked_at = %attestation.core.checked_at,
            "storage attestation verified"
        );
        Ok(VerifiedAttestation {
            attestation,
            cbor,
            verdicts,
        })
    }

    /// The one `POST /storage/verify` both forms send.
    async fn post_verify(
        &self,
        assets: &[AssetQuery],
        deep: bool,
        nonce: Option<&AttestationNonce>,
    ) -> Result<StorageVerifyResponseWire, VerifyError> {
        let body = StorageVerifyRequestWire {
            assets: assets
                .iter()
                .map(|a| AssetVerifyRequestWire {
                    asset_id: a.asset_id.to_string(),
                    blob_hashes: a.blob_hashes.iter().map(Hash32::to_hex).collect(),
                })
                .collect(),
            deep,
            attestation_nonce: nonce.map(|nonce| Hash32::from_bytes(*nonce).to_hex()),
        };
        let url = format!("{}/storage/verify", self.transport.base_url);
        let response = self
            .transport
            .send(|http| http.post(&url).json(&body))
            .await?;
        let response = check_status(response).await?;
        response
            .json()
            .await
            .map_err(|e| VerifyError::Malformed(e.to_string()))
    }

    /// `GET /upload/{id}/receipt`: the session-window custody-receipt fetch (pairs with lost-ACK
    /// recovery). Returns the decoded, signed receipt — verify it with
    /// [`capsule_core::library::verify_receipt`] before trusting it.
    ///
    /// The server serves the receipt as `application/cbor`: the body **is** the signed canonical
    /// encoding, so it is decoded from the bytes as they arrived and never round-trips through
    /// a JSON or base64 projection that could hand the verifier bytes the signature does not
    /// cover.
    #[instrument(skip_all, fields(%upload_id))]
    pub async fn fetch_receipt(&self, upload_id: Uuid) -> Result<CustodyReceipt, VerifyError> {
        let url = format!("{}/upload/{upload_id}/receipt", self.transport.base_url);
        let response = self.transport.send(|http| http.get(&url)).await?;
        let response = check_status(response).await?;
        let bytes = response
            .bytes()
            .await
            .map_err(|e| VerifyError::Transport(e.to_string()))?;
        debug!(len = bytes.len(), "custody receipt bytes received");
        decode_receipt_cbor(&bytes)
    }

    /// `GET /assets/{asset_id}/receipts`: the durable, permanent receipt log for an asset, in
    /// chain order. Each is signed — verify before trusting.
    #[instrument(skip_all, fields(%asset_id))]
    pub async fn fetch_asset_receipts(
        &self,
        asset_id: Uuid,
    ) -> Result<Vec<CustodyReceipt>, VerifyError> {
        let url = format!("{}/assets/{asset_id}/receipts", self.transport.base_url);
        let response = self.transport.send(|http| http.get(&url)).await?;
        let response = check_status(response).await?;
        let wire: AssetReceiptsResponseWire = response
            .json()
            .await
            .map_err(|e| VerifyError::Malformed(e.to_string()))?;
        wire.receipts
            .iter()
            .map(|r| decode_receipt_b64(&r.receipt_cbor))
            .collect()
    }

    /// Fetch the server's published attestation-key history from
    /// `GET {origin}/.well-known/capsule/attestation-keys`. This only fetches: the caller checks
    /// the result against the history it pinned for the server before trusting it
    /// (`capsule_core::library::pin_attestation_keys` — TOFU on first contact, append-only
    /// after). Every published key is returned so a receipt's `server_key_id` resolves against the whole
    /// history — a pre-rotation receipt still verifies.
    ///
    /// The record lives at the **origin** (RFC 8615), not under the API root the other methods
    /// use: the transport's `base_url` path (`/v1`) is replaced, not extended. A base that does
    /// not parse as a URL is a [`VerifyError::Transport`] raised before any request is sent.
    #[instrument(skip_all)]
    pub async fn fetch_attestation_keys(&self) -> Result<Vec<HybridVerifyingKey>, VerifyError> {
        let url = well_known_url(&self.transport.base_url, ATTESTATION_KEYS_PATH)?;
        debug!(%url, "fetching attestation keys");
        let response = self.transport.send(|http| http.get(url.clone())).await?;
        let response = check_status(response).await?;
        let wire: WellKnownAttestationWire = response
            .json()
            .await
            .map_err(|e| VerifyError::Malformed(e.to_string()))?;
        wire.keys
            .iter()
            .map(|k| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(k.public.as_bytes())
                    .map_err(|e| VerifyError::Malformed(format!("attestation key base64: {e}")))?;
                HybridVerifyingKey::from_bytes(&bytes)
                    .map_err(|e| VerifyError::Malformed(format!("attestation key: {e}")))
            })
            .collect()
    }
}

/// The attestation-key record's path, served at the server's origin
/// (`capsule-server/src/routes/well_known.rs`).
const ATTESTATION_KEYS_PATH: &str = "/.well-known/capsule/attestation-keys";

/// Resolve a `.well-known` `path` against the origin of the API root `base_url`: scheme, host
/// and port are kept, the API path (`/v1`) and any query are dropped.
fn well_known_url(base_url: &str, path: &str) -> Result<reqwest::Url, VerifyError> {
    reqwest::Url::parse(base_url)
        .and_then(|base| base.join(path))
        .map_err(|e| VerifyError::Transport(format!("base url {base_url:?}: {e}")))
}

/// Decode a receipt carried as base64 inside a JSON document (the asset-receipts chain).
fn decode_receipt_b64(b64: &str) -> Result<CustodyReceipt, VerifyError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| VerifyError::Malformed(format!("receipt base64: {e}")))?;
    decode_receipt_cbor(&bytes)
}

/// Decode the signed canonical-CBOR receipt bytes.
fn decode_receipt_cbor(bytes: &[u8]) -> Result<CustodyReceipt, VerifyError> {
    CustodyReceipt::from_canonical_cbor(bytes)
        .map_err(|e| VerifyError::Malformed(format!("receipt cbor: {e}")))
}

async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, VerifyError> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    // The stable `error.*` code rides the JSON body (like the upload/verify surfaces).
    let code = response
        .json::<ApiErrorWire>()
        .await
        .ok()
        .and_then(|e| e.code);
    Err(VerifyError::Status { status, code })
}

// ─── The 60-second re-verify window + release coordination ─────────────────────

/// The client's clock, injected so the re-verify window is deterministic under test.
pub trait ReleaseClock: Send + Sync {
    /// The current UNIX time in seconds.
    fn now_unix(&self) -> i64;
}

/// The wall clock (`jiff::Timestamp::now`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl ReleaseClock for SystemClock {
    fn now_unix(&self) -> i64 {
        jiff::Timestamp::now().as_second()
    }
}

/// The default re-verify window: a verdict older than this is re-fetched before any release
/// (SSoT: Storage Verification — "the client re-verifies if more than a bounded interval,
/// default 60 s, elapses between verdict and release").
pub const DEFAULT_REVERIFY_WINDOW_SECS: i64 = 60;

/// Everything the gate checks for one write: the asset, its required blob hashes, the upload
/// session that produced custody, and what the client sent (so the receipt's server-recomputed
/// facts can be matched).
#[derive(Debug, Clone)]
pub struct ReleaseRequest {
    /// The asset being released.
    pub asset_id: Uuid,
    /// The upload session whose receipt proves custody.
    pub upload_id: Uuid,
    /// Every required blob the client relies on (declared to `/storage/verify`).
    pub blob_hashes: Vec<Hash32>,
    /// What the client sent, matched against the receipt.
    pub expectations: ReceiptExpectations,
    /// Whether the offline `verify_asset` accepted the asset (the crypto-validity half).
    pub verify_asset_accepted: bool,
}

/// Composes durability, accountability, and crypto validity into a single release decision, and
/// enforces the 60-second re-verify window. One coordinator serves one asset's release attempts;
/// it caches the last verdict so a re-attempt inside the window skips a redundant round-trip.
pub struct ReleaseCoordinator<C: ReleaseClock = SystemClock> {
    client: StorageVerifyClient,
    attestation_keys: Vec<HybridVerifyingKey>,
    window_secs: i64,
    clock: C,
    /// The last verdict fetched and the UNIX time it was fetched at.
    cached: Mutex<Option<(StorageVerdict, i64)>>,
}

impl ReleaseCoordinator<SystemClock> {
    /// Build a coordinator over the wall clock with the default 60 s window.
    pub fn new(client: StorageVerifyClient, attestation_keys: Vec<HybridVerifyingKey>) -> Self {
        Self::with_clock(
            client,
            attestation_keys,
            DEFAULT_REVERIFY_WINDOW_SECS,
            SystemClock,
        )
    }
}

impl<C: ReleaseClock> ReleaseCoordinator<C> {
    /// Build a coordinator with an explicit window and clock (tests advance the clock to drive
    /// the re-verify window deterministically).
    pub fn with_clock(
        client: StorageVerifyClient,
        attestation_keys: Vec<HybridVerifyingKey>,
        window_secs: i64,
        clock: C,
    ) -> Self {
        Self {
            client,
            attestation_keys,
            window_secs,
            clock,
            cached: Mutex::new(None),
        }
    }

    /// Return a verdict fresh within the window: reuse a cached **durable** verdict younger than
    /// the window (the intended optimization for repeated release attempts), else re-fetch. Only
    /// durable verdicts are cached — a non-durable verdict must always be re-queried so a caller
    /// retrying after a `Retain` sees new server state, never a stale "not yet durable". The
    /// verdict is a point-in-time fact, so a stale one is never trusted for a release.
    async fn fresh_verdict(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<StorageVerdict, VerifyError> {
        let now = self.clock.now_unix();
        {
            let cached = self.cached.lock().expect("release cache mutex");
            if let Some((verdict, fetched_at)) = cached.as_ref()
                && verdict.asset_id == asset_id
                && now.saturating_sub(*fetched_at) < self.window_secs
            {
                debug!("reusing durable verdict within the re-verify window");
                return Ok(verdict.clone());
            }
        }
        let query = AssetQuery {
            asset_id,
            blob_hashes: blob_hashes.to_vec(),
        };
        let mut verdicts = self.client.verify(&[query], false).await?;
        let verdict = verdicts
            .drain(..)
            .find(|v| v.asset_id == asset_id)
            .ok_or_else(|| VerifyError::Malformed("verdict missing the requested asset".into()))?;
        let mut cached = self.cached.lock().expect("release cache mutex");
        *cached = if verdict.durable {
            Some((verdict.clone(), now))
        } else {
            None
        };
        Ok(verdict)
    }

    /// Evaluate the full verify-before-destroy gate for one write. Returns
    /// [`ReleaseDecision::Release`] only when the verdict is durable within the window, the
    /// custody receipt verifies under a pinned attestation key with matching fields, and
    /// `verify_asset` accepted the asset. Every other outcome is a `Retain` that names why — the
    /// caller keeps the local copy and retries.
    #[instrument(skip_all, fields(asset = %request.asset_id))]
    pub async fn evaluate(&self, request: &ReleaseRequest) -> ReleaseDecision {
        // 1. Durability, fresh within the window.
        let verdict = match self
            .fresh_verdict(request.asset_id, &request.blob_hashes)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "verdict fetch failed; retaining local copy");
                return ReleaseDecision::Retain(RetainReason::VerifyUnavailable);
            }
        };
        if !release_is_safe(&verdict, request.verify_asset_accepted) {
            return ReleaseDecision::Retain(RetainReason::NotDurable);
        }

        // 2. Accountability: fetch + verify the custody receipt under the pinned key.
        let receipt = match self.client.fetch_receipt(request.upload_id).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "receipt fetch failed; refusing release");
                return ReleaseDecision::Retain(RetainReason::ReceiptUnavailable);
            }
        };
        match verify_receipt(
            &receipt,
            &self.attestation_keys,
            &request.expectations,
            self.clock.now_unix(),
        ) {
            Ok(()) => ReleaseDecision::Release,
            Err(rejection) => {
                // Every receipt rejection (bad signature, field mismatch, clock drift) refuses
                // release — the reason surfaced to the caller is the unverified receipt.
                warn!(
                    ?rejection,
                    "custody receipt did not verify; refusing release"
                );
                ReleaseDecision::Retain(RetainReason::ReceiptMissing)
            }
        }
    }

    /// The verified custody receipt for a write, for the caller to persist beside the provenance
    /// chain (`capsule_core::library::append_receipt`) — evidence, not a cache. Returns the
    /// receipt only when it verifies under a pinned attestation key with matching fields.
    #[instrument(skip_all, fields(%upload_id))]
    pub async fn fetch_verified_receipt(
        &self,
        upload_id: Uuid,
        expectations: &ReceiptExpectations,
    ) -> Result<CustodyReceipt, VerifyError> {
        let receipt = self.client.fetch_receipt(upload_id).await?;
        verify_receipt(
            &receipt,
            &self.attestation_keys,
            expectations,
            self.clock.now_unix(),
        )
        .map_err(|r| VerifyError::Malformed(format!("receipt rejected: {r:?}")))?;
        Ok(receipt)
    }

    /// A signed durability verdict for `assets`, under a nonce drawn fresh from the OS CSPRNG
    /// and verified under the pinned attestation keys — the evidence form of the verdict
    /// (transfer/durability.md, Signed Storage Attestation). The caller keeps
    /// [`VerifiedAttestation::cbor`] beside the asset's custody receipts.
    ///
    /// Not part of [`evaluate`](Self::evaluate): the release gate needs a fresh `durable`, not
    /// evidence, and a signed request spends the account's deep budget.
    #[instrument(skip_all, fields(assets = assets.len(), deep))]
    pub async fn fetch_verified_attestation(
        &self,
        assets: &[AssetQuery],
        deep: bool,
    ) -> Result<VerifiedAttestation, VerifyError> {
        let nonce = capsule_core::crypto::rng::random_array::<ATTESTATION_NONCE_LEN>();
        self.client
            .verify_signed(assets, deep, &nonce, &self.attestation_keys)
            .await
    }

    /// Fetch the server-side facts the release gate needs for each of `releases`, and hand
    /// them back as a [`CustodyVerifier`] — the production
    /// [`StorageVerifier`] the offline data plane's gate
    /// ([`Workspace::release_move_source`](capsule_core::lifecycle::Workspace::release_move_source))
    /// consults synchronously.
    ///
    /// Per asset: a verdict fresh within the re-verify window over its declared blobs, then —
    /// only when that verdict is `durable` — the asset's durable receipt log
    /// (`GET /assets/{id}/receipts`), with every declared blob required to have a receipt that
    /// verifies under the pinned attestation keys with fields matching what the client sent.
    /// The log rather than the per-session fetch [`evaluate`](Self::evaluate) uses, because a
    /// blob a push found already held has no upload session to ask about.
    ///
    /// Never fails: a fetch that fails is recorded as that asset's fact, and the gate turns it
    /// into a `Retain`. Resolve immediately before releasing; the facts are point-in-time.
    #[instrument(skip_all, fields(assets = releases.len()))]
    pub async fn resolve_custody(&self, releases: &[HeldRelease]) -> CustodyVerifier {
        let mut facts = HashMap::with_capacity(releases.len());
        for release in releases {
            let blob_hashes = release.blob_hashes();
            let verdict = self
                .fresh_verdict(release.asset_id, &blob_hashes)
                .await
                .map_err(|error| {
                    warn!(asset = %release.asset_id, %error, "custody: verdict fetch failed");
                    error.to_string()
                });
            let receipt = match &verdict {
                Ok(verdict) if verdict.durable => self.receipt_log_covers(release).await,
                Ok(_) => Err("not consulted: the verdict is not durable".to_owned()),
                Err(_) => Err("not consulted: no verdict".to_owned()),
            };
            debug!(
                asset = %release.asset_id,
                durable = verdict.as_ref().is_ok_and(|v| v.durable),
                receipts_verified = ?receipt,
                "custody: facts resolved"
            );
            facts.insert(
                release.asset_id,
                CustodyFacts {
                    blob_hashes,
                    verdict,
                    receipt,
                },
            );
        }
        CustodyVerifier { facts }
    }

    /// Whether the asset's receipt log holds, for every blob `release` declares, a receipt
    /// that verifies under the pinned keys with matching fields. `Err` when the log fetch fails.
    async fn receipt_log_covers(&self, release: &HeldRelease) -> Result<bool, String> {
        let log = self
            .client
            .fetch_asset_receipts(release.asset_id)
            .await
            .map_err(|error| {
                warn!(asset = %release.asset_id, %error, "custody: receipt log fetch failed");
                error.to_string()
            })?;
        let now = self.clock.now_unix();
        let missing: Vec<String> = release
            .blobs
            .iter()
            .filter(|expected| {
                !log.iter().any(|receipt| {
                    verify_receipt(receipt, &self.attestation_keys, expected, now).is_ok()
                })
            })
            .map(|expected| expected.ciphertext_hash.to_hex())
            .collect();
        if !missing.is_empty() {
            warn!(
                asset = %release.asset_id,
                ?missing,
                log = log.len(),
                "custody: no verified receipt for every declared blob"
            );
        }
        Ok(missing.is_empty())
    }
}

// ─── The production release-gate verifier ─────────────────────────────────────

/// One asset whose local bytes wait on server custody before they may be released, asked
/// about through [`ReleaseCoordinator::resolve_custody`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldRelease {
    /// The asset.
    pub asset_id: Uuid,
    /// One entry per required blob: what the client sent, matched against the receipt log.
    /// Their `ciphertext_hash`es, in order, are the blobs declared to `/storage/verify`.
    pub blobs: Vec<ReceiptExpectations>,
}

impl HeldRelease {
    /// The blobs a release of `bundle`'s original waits on: the original ciphertext and, when
    /// the head binds one, the sealed metadata blob — in the order
    /// [`Workspace::release_move_source`](capsule_core::lifecycle::Workspace::release_move_source)
    /// declares them.
    #[must_use]
    pub fn for_original(bundle: &UploadBundle) -> Self {
        let mut blobs = vec![ReceiptExpectations {
            ciphertext_hash: bundle.ciphertext_hash,
            size: bundle.ciphertext_size(),
            role: BlobRole::Original,
            envelope_hash: None,
        }];
        if let Some(hash) = bundle.metadata_blob_hash {
            blobs.push(ReceiptExpectations {
                ciphertext_hash: hash,
                size: bundle.metadata_blob.len() as u64,
                role: BlobRole::Metadata,
                envelope_hash: None,
            });
        }
        Self {
            asset_id: bundle.asset_id,
            blobs,
        }
    }

    fn blob_hashes(&self) -> Vec<Hash32> {
        self.blobs.iter().map(|b| b.ciphertext_hash).collect()
    }
}

/// What [`ReleaseCoordinator::resolve_custody`] fetched for one asset. A fetch that failed is
/// kept as its English detail.
#[derive(Debug, Clone)]
struct CustodyFacts {
    blob_hashes: Vec<Hash32>,
    verdict: Result<StorageVerdict, String>,
    receipt: Result<bool, String>,
}

/// The production [`StorageVerifier`]: server facts resolved over [`StorageVerifyClient`] by
/// [`ReleaseCoordinator::resolve_custody`], answered synchronously to the offline gate.
///
/// It answers only what was resolved. An asset it was not asked about, or the right asset with
/// a different blob set, is a [`VerifierError`] — which the gate turns into a `Retain` — so
/// nothing is ever released on a verdict that answered another question.
#[derive(Debug, Clone, Default)]
pub struct CustodyVerifier {
    facts: HashMap<Uuid, CustodyFacts>,
}

impl CustodyVerifier {
    fn facts(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<&CustodyFacts, VerifierError> {
        match self.facts.get(&asset_id) {
            Some(facts) if facts.blob_hashes == blob_hashes => Ok(facts),
            Some(_) => Err(VerifierError::Transport(format!(
                "custody for {asset_id} was resolved over a different blob set"
            ))),
            None => Err(VerifierError::Transport(format!(
                "custody for {asset_id} was not resolved"
            ))),
        }
    }
}

impl StorageVerifier for CustodyVerifier {
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<StorageVerdict, VerifierError> {
        self.facts(asset_id, blob_hashes)?
            .verdict
            .clone()
            .map_err(VerifierError::Transport)
    }

    fn receipt_verified(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<bool, VerifierError> {
        self.facts(asset_id, blob_hashes)?
            .receipt
            .clone()
            .map_err(VerifierError::Transport)
    }
}

#[cfg(test)]
mod tests;
