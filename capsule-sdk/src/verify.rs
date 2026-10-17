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
//!    [`CustodyReceipt`](capsule_core::library::CustodyReceipt) and
//!    [`capsule_core::library::verify_receipt`] checks the hybrid signature under the pinned
//!    attestation key plus a field match against what the client sent. A server that withholds
//!    receipts never becomes the sole holder of an only-copy.
//! 3. **Crypto validity.** `verify_asset` (the caller's offline check) must have accepted the
//!    asset — passed in as `verify_asset_accepted`.
//!
//! [`ReleaseCoordinator`] composes the three and enforces the **60-second re-verify window**:
//! the verdict is a point-in-time fact, so a verdict older than the window is re-fetched before
//! release. The result is a [`capsule_core::library::ReleaseDecision`]; a non-`Release` outcome
//! never destroys — the caller retains the local copy, retries with backoff, and surfaces
//! "not yet confirmed on server". Streaming import (`S-B3`) drives this per asset in its
//! import→upload→verify→release window.
//!
//! [Storage Verification — Verify Before Destroy]: https://docs/design/import/storage-verification/

use std::sync::Mutex;

use base64::Engine as _;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::HybridVerifyingKey;
use capsule_core::library::{
    BlobRole, BlobVerdict, CustodyReceipt, ReceiptExpectations, ReleaseDecision, RetainReason,
    StorageVerdict, release_is_safe, verify_receipt,
};
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
}

impl From<crate::auth::AuthError> for VerifyError {
    fn from(err: crate::auth::AuthError) -> Self {
        VerifyError::Transport(err.to_string())
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
    /// `GET {base}/upload/{id}/receipt`, `GET {base}/assets/{id}/receipts`).
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
            VerifyAuth::Static { http, token } => build(http)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| VerifyError::Transport(e.to_string())),
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

/// The receipt fetch response (both `GET /upload/{id}/receipt` and `GET /assets/{id}/receipts`
/// carry `receipt_cbor` as base64 of the full signed canonical-CBOR receipt).
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
        let body = StorageVerifyRequestWire {
            assets: assets
                .iter()
                .map(|a| AssetVerifyRequestWire {
                    asset_id: a.asset_id.to_string(),
                    blob_hashes: a.blob_hashes.iter().map(Hash32::to_hex).collect(),
                })
                .collect(),
            deep,
        };
        let url = format!("{}/storage/verify", self.transport.base_url);
        let response = self
            .transport
            .send(|http| http.post(&url).json(&body))
            .await?;
        let response = check_status(response).await?;
        let wire: StorageVerifyResponseWire = response
            .json()
            .await
            .map_err(|e| VerifyError::Malformed(e.to_string()))?;
        wire.verdicts
            .into_iter()
            .map(StorageVerdictWire::into_core)
            .collect()
    }

    /// `GET /upload/{id}/receipt`: the session-window custody-receipt fetch (pairs with lost-ACK
    /// recovery). Returns the decoded, signed receipt — verify it with
    /// [`capsule_core::library::verify_receipt`] before trusting it.
    #[instrument(skip_all, fields(%upload_id))]
    pub async fn fetch_receipt(&self, upload_id: Uuid) -> Result<CustodyReceipt, VerifyError> {
        let url = format!("{}/upload/{upload_id}/receipt", self.transport.base_url);
        let response = self.transport.send(|http| http.get(&url)).await?;
        let response = check_status(response).await?;
        let wire: ReceiptWire = response
            .json()
            .await
            .map_err(|e| VerifyError::Malformed(e.to_string()))?;
        decode_receipt(&wire.receipt_cbor)
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
            .map(|r| decode_receipt(&r.receipt_cbor))
            .collect()
    }

    /// Fetch and pin the server's attestation-key history from
    /// `GET {well_known_base}/.well-known/capsule/attestation-keys` (TOFU on first contact).
    /// Every published key is returned so a receipt's `server_key_id` resolves against the whole
    /// history — a pre-rotation receipt still verifies.
    #[instrument(skip_all)]
    pub async fn fetch_attestation_keys(&self) -> Result<Vec<HybridVerifyingKey>, VerifyError> {
        let url = format!(
            "{}/.well-known/capsule/attestation-keys",
            self.transport.base_url
        );
        let response = self.transport.send(|http| http.get(&url)).await?;
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

fn decode_receipt(b64: &str) -> Result<CustodyReceipt, VerifyError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| VerifyError::Malformed(format!("receipt base64: {e}")))?;
    CustodyReceipt::from_canonical_cbor(&bytes)
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
    async fn fresh_verdict(&self, request: &ReleaseRequest) -> Result<StorageVerdict, VerifyError> {
        let now = self.clock.now_unix();
        {
            let cached = self.cached.lock().expect("release cache mutex");
            if let Some((verdict, fetched_at)) = cached.as_ref()
                && verdict.asset_id == request.asset_id
                && now.saturating_sub(*fetched_at) < self.window_secs
            {
                debug!("reusing durable verdict within the re-verify window");
                return Ok(verdict.clone());
            }
        }
        let query = AssetQuery {
            asset_id: request.asset_id,
            blob_hashes: request.blob_hashes.clone(),
        };
        let mut verdicts = self.client.verify(&[query], false).await?;
        let verdict = verdicts
            .drain(..)
            .find(|v| v.asset_id == request.asset_id)
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
        let verdict = match self.fresh_verdict(request).await {
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
}

#[cfg(test)]
mod tests;
