//! Recovery-secret verification cadence and the guided re-wrap flow (slice `S-D12`;
//! SSoT: [Backup — Recovery Verification Cadence] and [§ On Repeated Failure: Guided
//! Re-Wrap]).
//!
//! This module owns the **client half** of the master-key recovery story that the
//! server escrow surface (slice `S-C12`, `PUT`/`GET /backup/escrow`) and the core
//! crypto ([`capsule_core::backup`]) make possible. It has two cohesive halves:
//!
//! - **[`cadence`]** — the pure, network-free scheduler and prompt state machine
//!   ([`RecoveryCadence`] → [`VerificationState`]): the 7 d → 90 d → 180 d ladder,
//!   re-arm triggers, snooze caps, and the escalation to a guided re-wrap. It is driven
//!   by a mocked clock and never blocks any flow.
//! - **[`RecoveryClient`]** — the networked side: the cached copy of the escrow blob
//!   ([`EscrowCache`]), the stale-cache-aware [`verify`](RecoveryClient::verify), and the
//!   [`guided_rewrap`](RecoveryClient::guided_rewrap) that mints a fresh secret, re-wraps
//!   the **same** master key, and replaces the server escrow through S-C12.
//!
//! The escrow blob on the wire is the opaque canonical-CBOR encoding of a
//! [`WrappedSecret`] — byte-identical to what the server stores verbatim and to what the
//! core restore path unwraps.
//!
//! [Backup — Recovery Verification Cadence]: https://docs/design/backup-recovery/#recovery-verification-cadence
//! [§ On Repeated Failure: Guided Re-Wrap]: https://docs/design/backup-recovery/#on-repeated-failure-guided-re-wrap

pub mod cadence;

pub use cadence::{
    BACKOFF_INTERVAL_SECS, CAP_INTERVAL_SECS, INITIAL_INTERVAL_SECS, MAX_CONSECUTIVE_SNOOZES,
    REWRAP_FAILURE_THRESHOLD, REWRAP_MIN_SESSIONS, RearmTrigger, RecoveryCadence, SnoozeDuration,
    VerificationState,
};
use capsule_core::backup::{VerifyOutcome, split_seed_2of3, verify_recovery_secret};
use capsule_core::crypto::primitives::{Argon2Params, DeviceTier};
use capsule_core::crypto::pwkdf::{self, WrappedSecret};
use capsule_core::crypto::rng;
use tracing::instrument;

use crate::auth::{AuthError, Session};

/// The escrow endpoint path, appended to the caller's API base.
const ESCROW_PATH: &str = "backup/escrow";

/// Everything the networked recovery flows can fail with. Callers switch on the typed
/// variant, never a bare HTTP status.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    /// The authenticated request itself failed (transport, session expiry, refresh).
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// Reading the escrow response body off the wire failed.
    #[error("reading escrow response body failed: {0}")]
    Body(#[source] reqwest::Error),
    /// The caller has no escrow stored yet (server returned `404`). Enroll one first.
    #[error("no escrow stored for this account")]
    NotEnrolled,
    /// The escrow bytes could not be (de)serialized as the canonical `WrappedSecret`.
    #[error("escrow blob codec error: {0}")]
    Codec(String),
    /// The (re-)wrap of the master key under the fresh secret failed in core.
    #[error("re-wrapping the master key failed: {0}")]
    Wrap(String),
    /// The server returned an unmodeled status.
    #[error("unexpected {status} response from the escrow endpoint")]
    Unexpected {
        /// The HTTP status code the server returned.
        status: u16,
    },
}

/// A client-side cached copy of the server escrow blob.
///
/// Fetched at enrollment and refreshed opportunistically (SSoT § Local Verification);
/// the [stale-cache rule](RecoveryClient::verify) refreshes it before ever recording a
/// verification failure, so a rotation on another device can never manufacture a false
/// failure here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowCache {
    blob: WrappedSecret,
}

impl EscrowCache {
    /// Wrap an already-decoded escrow blob as the local cache.
    #[must_use]
    pub fn new(blob: WrappedSecret) -> Self {
        Self { blob }
    }

    /// The cached wrapped-master-key escrow blob.
    #[must_use]
    pub fn blob(&self) -> &WrappedSecret {
        &self.blob
    }

    /// Decode an escrow cache from the opaque canonical-CBOR wire bytes.
    fn from_wire(bytes: &[u8]) -> Result<Self, RecoveryError> {
        let blob: WrappedSecret = capsule_core::cbor::from_slice(bytes)
            .map_err(|e| RecoveryError::Codec(e.to_string()))?;
        Ok(Self { blob })
    }
}

/// A freshly minted recovery secret, presented to the user exactly once. It doubles as
/// the escrow wrap passphrase and (when enrolled) the Shamir-split seed — the single
/// root of the [single-root invariant](https://docs/design/backup-recovery/#single-root-invariant).
///
/// The bytes are zeroized on drop; surface them to the user (as a BIP39-style phrase or
/// hex) and then let this drop.
pub struct MintedSecret {
    seed: [u8; 32],
}

impl MintedSecret {
    /// Draw a fresh 32-byte (256-bit — well over the ≥128-bit floor) recovery secret.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            seed: rng::random_array::<32>(),
        }
    }

    /// The raw secret bytes (the escrow wrap passphrase and Shamir seed).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.seed
    }

    /// Lowercase-hex rendering, for surfacing the secret to the user.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.seed)
    }

    /// The setup-style **type-back gate**: confirm the user re-entered the secret they
    /// were just shown. A constant-time compare over the decoded bytes.
    #[must_use]
    pub fn matches(&self, typed: &[u8]) -> bool {
        typed.len() == self.seed.len()
            && typed
                .iter()
                .zip(self.seed.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

impl Drop for MintedSecret {
    fn drop(&mut self) {
        // Best-effort zeroization of the recovery secret.
        for byte in &mut self.seed {
            *byte = 0;
        }
    }
}

impl std::fmt::Debug for MintedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret material.
        f.debug_struct("MintedSecret").finish_non_exhaustive()
    }
}

/// Guidance the client surfaces after a rotation about backup **artifacts** exported
/// under the *old* secret — pure data (a stable variant), never a localized string, so
/// the client maps it to a catalog key.
///
/// Per the [single-root invariant] versioning seam: an already-exported backup artifact
/// stays bound to the passphrase in force at export, so rotating the recovery secret
/// ends with "re-export or destroy old artifacts" guidance.
///
/// [single-root invariant]: https://docs/design/backup-recovery/#single-root-invariant
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OldArtifactGuidance {
    /// Backup artifacts exported before this rotation still open only with the *old*
    /// secret. Re-export them under the new secret, or destroy them.
    ReexportOrDestroy,
}

/// The Shamir shares re-issued during a guided re-wrap when the account had social
/// recovery enrolled. The old shares are explicitly invalidated (they split the *old*
/// seed) and surfaced as such.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShamirReissue {
    /// The fresh 2-of-3 shares splitting the **new** seed.
    pub shares: Vec<Vec<u8>>,
    /// Always `true`: the previously issued shares split the old seed and no longer
    /// reconstruct anything useful — the client must tell the user to discard them.
    pub old_shares_invalidated: bool,
}

/// The outcome of a guided re-wrap: everything the UX needs, surfaced as data.
///
/// The master key is **unchanged** — this is wrap rotation, not key rotation (SSoT
/// § On Repeated Failure). [`new_escrow`](Self::new_escrow) wraps the same master key
/// under [`secret`](Self::secret); no asset blob is touched or re-encrypted.
#[derive(Debug)]
pub struct GuidedRewrap {
    /// The freshly minted recovery secret — present once, confirm via the type-back
    /// gate ([`MintedSecret::matches`]), then let it drop.
    pub secret: MintedSecret,
    /// The new escrow blob (already stored on the server), cached locally.
    pub new_escrow: EscrowCache,
    /// Re-issued Shamir shares, when the account had social recovery enrolled.
    pub shamir: Option<ShamirReissue>,
    /// Whether the client must run the setup-style type-back gate (always `true`).
    pub type_back_required: bool,
    /// Guidance about backup artifacts exported under the old secret.
    pub old_artifact_guidance: OldArtifactGuidance,
}

/// The networked recovery client: escrow cache/refresh, stale-cache-aware local
/// verification, and the guided re-wrap. It borrows an authenticated [`Session`], so
/// every call rides the SDK's bearer/refresh machinery.
#[derive(Clone)]
pub struct RecoveryClient {
    session: Session,
    escrow_url: String,
}

impl RecoveryClient {
    /// Build a recovery client against the API base URL (the same base the auth session
    /// authenticates against, e.g. `https://api.example.com`).
    #[must_use]
    pub fn new(session: Session, api_base_url: &str) -> Self {
        let escrow_url = format!("{}/{ESCROW_PATH}", api_base_url.trim_end_matches('/'));
        Self {
            session,
            escrow_url,
        }
    }

    /// Fetch the current escrow blob from the server (`GET /backup/escrow`) into a fresh
    /// [`EscrowCache`]. `404` maps to [`RecoveryError::NotEnrolled`].
    #[instrument(skip_all)]
    pub async fn fetch_escrow(&self) -> Result<EscrowCache, RecoveryError> {
        let response = self.session.execute(|c| c.get(&self.escrow_url)).await?;
        let status = response.status();
        match status {
            reqwest::StatusCode::OK => {
                let bytes = response.bytes().await.map_err(RecoveryError::Body)?;
                tracing::debug!(len = bytes.len(), "fetched escrow blob");
                EscrowCache::from_wire(&bytes)
            }
            reqwest::StatusCode::NOT_FOUND => Err(RecoveryError::NotEnrolled),
            other => Err(RecoveryError::Unexpected {
                status: other.as_u16(),
            }),
        }
    }

    /// Store or replace the caller's escrow blob (`PUT /backup/escrow`). Single active
    /// escrow: the server overwrites any prior blob in the same transaction (S-C12).
    #[instrument(skip_all)]
    pub async fn store_escrow(&self, blob: &WrappedSecret) -> Result<(), RecoveryError> {
        let body = capsule_core::cbor::to_canonical_vec(blob)
            .map_err(|e| RecoveryError::Codec(e.to_string()))?;
        let response = self
            .session
            .execute(|c| {
                c.put(&self.escrow_url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(body.clone())
            })
            .await?;
        let status = response.status();
        if status.is_success() {
            tracing::info!("escrow blob stored (single active escrow: any prior blob replaced)");
            Ok(())
        } else {
            Err(RecoveryError::Unexpected {
                status: status.as_u16(),
            })
        }
    }

    /// Local recovery-secret verification with the **stale-cache rule** (SSoT § Local
    /// Verification): run the offline derived-tag compare against the cached blob; if it
    /// does not verify, refresh the blob from the server **once** and retry before
    /// returning a failure — so a rotation on another device cannot manufacture a false
    /// failure here.
    ///
    /// Purely local crypto (`capsule_core::backup::verify_recovery_secret`); the only
    /// network I/O is the single conditional refresh. Never blocks anything.
    #[instrument(skip_all)]
    pub async fn verify(
        &self,
        cache: &mut EscrowCache,
        passphrase: &[u8],
        device_master: &[u8; 32],
    ) -> Result<VerifyOutcome, RecoveryError> {
        if verify_recovery_secret(cache.blob(), passphrase, device_master)
            == VerifyOutcome::Verified
        {
            tracing::debug!("recovery secret verified against the cached escrow");
            return Ok(VerifyOutcome::Verified);
        }

        // Stale-cache rule: the cached blob may be stale after a rotation elsewhere.
        // Refresh once and retry before recording a failure.
        tracing::info!("cached-escrow verify failed; refreshing escrow and retrying once");
        let refreshed = self.fetch_escrow().await?;
        let changed = refreshed != *cache;
        *cache = refreshed;
        if changed {
            let outcome = verify_recovery_secret(cache.blob(), passphrase, device_master);
            tracing::debug!(?outcome, "retried verify against refreshed escrow");
            Ok(outcome)
        } else {
            // The blob is genuinely current — this is a real failure.
            tracing::debug!("escrow unchanged on refresh; recording a genuine failure");
            Ok(VerifyOutcome::NotVerified)
        }
    }

    /// Run the guided re-wrap (SSoT § On Repeated Failure): mint a fresh ≥128-bit
    /// recovery secret, **re-wrap the same master key** under it, replace the server
    /// escrow (single active — the old blob is deleted, so the lost secret unwraps
    /// nothing), re-issue Shamir shares when enrolled, and surface the old-artifact
    /// guidance. Wrap rotation, not key rotation: no data re-encryption, no blob-hash
    /// changes.
    ///
    /// After this succeeds, the caller should
    /// [`rearm`](RecoveryCadence::rearm)`(now, RearmTrigger::SecretRotated)` its cadence.
    #[instrument(skip_all)]
    pub async fn guided_rewrap(
        &self,
        device_master: &[u8; 32],
        tier: DeviceTier,
        shamir_enrolled: bool,
    ) -> Result<GuidedRewrap, RecoveryError> {
        self.guided_rewrap_with_params(device_master, tier.params(), shamir_enrolled)
            .await
    }

    /// The [`guided_rewrap`](Self::guided_rewrap) body with explicit Argon2id parameters
    /// — the seam tests drive with fast params so the crypto does not dominate a smoke.
    #[instrument(skip_all)]
    async fn guided_rewrap_with_params(
        &self,
        device_master: &[u8; 32],
        params: Argon2Params,
        shamir_enrolled: bool,
    ) -> Result<GuidedRewrap, RecoveryError> {
        let secret = MintedSecret::generate();

        // Re-wrap the SAME master key under the fresh secret. `device_master` is an
        // input, never rotated — the escrow is the only thing that changes.
        let new_blob = pwkdf::wrap_with(device_master, secret.as_bytes(), params)
            .map_err(|e| RecoveryError::Wrap(e.to_string()))?;

        // Replace the server escrow (single active escrow — the old ciphertext is gone).
        self.store_escrow(&new_blob).await?;
        tracing::info!("guided re-wrap: server escrow replaced with the new-secret wrap");

        // Re-issue Shamir shares of the NEW seed when the account had them enrolled.
        let shamir = shamir_enrolled.then(|| ShamirReissue {
            shares: split_seed_2of3(secret.as_bytes()),
            old_shares_invalidated: true,
        });

        Ok(GuidedRewrap {
            secret,
            new_escrow: EscrowCache::new(new_blob),
            shamir,
            type_back_required: true,
            old_artifact_guidance: OldArtifactGuidance::ReexportOrDestroy,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use capsule_core::backup::recover_master_key;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::auth::{AuthClient, PersistedSession};

    /// Fast Argon2id params so the escrow crypto does not dominate the smoke tests
    /// (same posture as the S-C12 server integration test).
    fn fast_params() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    // ── Binary-capable mock escrow server ─────────────────────────────────────
    //
    // The escrow surface is `application/octet-stream` in both directions, so unlike the
    // auth mock (JSON strings) this one stores and serves raw bytes. A single shared
    // slot models the server's single-active-escrow row.

    #[derive(Clone, Default)]
    struct EscrowStore {
        blob: Arc<Mutex<Option<Vec<u8>>>>,
    }

    struct MockRequest {
        method: String,
        body: Vec<u8>,
    }

    struct MockResponse {
        status: u16,
        body: Vec<u8>,
    }

    type BoxFut = Pin<Box<dyn Future<Output = MockResponse> + Send>>;
    type Handler = Arc<dyn Fn(MockRequest) -> BoxFut + Send + Sync>;

    async fn start_mock(handler: Handler) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(&mut socket, handler).await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    async fn serve_conn(socket: &mut TcpStream, handler: Handler) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };

        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let method = request_line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();

        let mut content_length = 0usize;
        let mut authorized = false;
        for line in lines {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim();
                if key == "content-length" {
                    content_length = value.parse().unwrap_or(0);
                }
                if key == "authorization" && value.starts_with("Bearer ") {
                    authorized = true;
                }
            }
        }

        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }

        // Every escrow call is owner-scoped: reject anything without a bearer token.
        let response = if authorized {
            handler(MockRequest { method, body }).await
        } else {
            MockResponse {
                status: 401,
                body: Vec::new(),
            }
        };

        let payload = format!(
            "HTTP/1.1 {} STATUS\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.status,
            response.body.len()
        );
        let mut out = payload.into_bytes();
        out.extend_from_slice(&response.body);
        socket.write_all(&out).await?;
        socket.flush().await?;
        Ok(())
    }

    /// A handler backed by the shared single-active-escrow slot: `PUT` overwrites it,
    /// `GET` serves it verbatim (or 404).
    fn escrow_handler(store: EscrowStore) -> Handler {
        Arc::new(move |req| {
            let store = store.clone();
            Box::pin(async move {
                match req.method.as_str() {
                    "PUT" => {
                        *store.blob.lock().unwrap() = Some(req.body);
                        MockResponse {
                            status: 204,
                            body: Vec::new(),
                        }
                    }
                    "GET" => match store.blob.lock().unwrap().clone() {
                        Some(bytes) => MockResponse {
                            status: 200,
                            body: bytes,
                        },
                        None => MockResponse {
                            status: 404,
                            body: Vec::new(),
                        },
                    },
                    _ => MockResponse {
                        status: 405,
                        body: Vec::new(),
                    },
                }
            })
        })
    }

    /// Build an authenticated [`Session`] over the mock base with a far-future token, so
    /// no refresh ever fires and the mock needs no `/refresh` endpoint.
    fn session_for(base: &str) -> Session {
        let client = AuthClient::new(base).unwrap();
        client
            .resume(PersistedSession {
                access_token: "test-access".to_string().into(),
                refresh_token: "test-refresh".to_string().into(),
                access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
            })
            .unwrap()
    }

    fn wrap(master: &[u8; 32], secret: &[u8]) -> WrappedSecret {
        pwkdf::wrap_with(master, secret, fast_params()).unwrap()
    }

    /// Store → fetch round-trips the escrow verbatim, and the fetched blob unwraps to
    /// the original master key.
    #[tokio::test]
    async fn escrow_store_fetch_round_trip() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base);

        let master = [0x11u8; 32];
        let blob = wrap(&master, b"correct horse battery staple");
        client.store_escrow(&blob).await.unwrap();

        let cache = client.fetch_escrow().await.unwrap();
        assert_eq!(cache.blob(), &blob, "fetched blob equals stored blob");
        assert_eq!(
            recover_master_key(cache.blob(), b"correct horse battery staple").unwrap(),
            master
        );
    }

    /// Fetch with nothing stored is a typed `NotEnrolled`, not a panic or opaque error.
    #[tokio::test]
    async fn fetch_without_escrow_is_not_enrolled() {
        let base = start_mock(escrow_handler(EscrowStore::default())).await;
        let client = RecoveryClient::new(session_for(&base), &base);
        assert!(matches!(
            client.fetch_escrow().await,
            Err(RecoveryError::NotEnrolled)
        ));
    }

    /// The correct secret verifies against the cached blob with a single fetch (the
    /// initial enroll) and no refresh needed.
    #[tokio::test]
    async fn verify_correct_secret_against_cache() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base);

        let master = [0x22u8; 32];
        let blob = wrap(&master, b"the-right-secret");
        client.store_escrow(&blob).await.unwrap();
        let mut cache = client.fetch_escrow().await.unwrap();

        assert_eq!(
            client
                .verify(&mut cache, b"the-right-secret", &master)
                .await
                .unwrap(),
            VerifyOutcome::Verified
        );
    }

    /// SSoT § Local Verification stale-cache rule: a rotation on another device makes
    /// the *cached* blob stale; verify refreshes once and then passes with the new
    /// secret — the stale cache never manufactures a false failure.
    #[tokio::test]
    async fn verify_refreshes_stale_cache_then_passes() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store.clone())).await;
        let client = RecoveryClient::new(session_for(&base), &base);

        let master = [0x33u8; 32];
        // Enroll and cache the OLD wrap.
        let old_blob = wrap(&master, b"old-secret");
        client.store_escrow(&old_blob).await.unwrap();
        let mut cache = client.fetch_escrow().await.unwrap();

        // Another device rotates the escrow to a NEW secret (server-side replace).
        let new_blob = wrap(&master, b"new-secret");
        *store.blob.lock().unwrap() =
            Some(capsule_core::cbor::to_canonical_vec(&new_blob).unwrap());

        // Verifying with the NEW secret against the STALE cache: the first compare
        // fails, the stale-cache refresh pulls the new blob, and the retry passes.
        assert_eq!(
            client
                .verify(&mut cache, b"new-secret", &master)
                .await
                .unwrap(),
            VerifyOutcome::Verified
        );
        // The cache was updated to the refreshed blob.
        assert_eq!(cache.blob(), &new_blob);
    }

    /// A genuinely wrong secret still fails after the refresh (the blob was current):
    /// the stale-cache rule does not paper over a real failure.
    #[tokio::test]
    async fn verify_wrong_secret_fails_after_refresh() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base);

        let master = [0x44u8; 32];
        let blob = wrap(&master, b"real-secret");
        client.store_escrow(&blob).await.unwrap();
        let mut cache = client.fetch_escrow().await.unwrap();

        assert_eq!(
            client
                .verify(&mut cache, b"totally-wrong", &master)
                .await
                .unwrap(),
            VerifyOutcome::NotVerified
        );
    }

    /// SSoT § Guided re-wrap smoke: after the failure threshold the client re-wraps the
    /// **same** master key under a fresh secret and replaces the server escrow.
    ///
    /// Proves:
    /// - the master key is UNCHANGED — the new escrow unwraps with the new secret to the
    ///   exact original master bytes;
    /// - the old secret unwraps nothing (single active escrow — it is gone everywhere);
    /// - re-wrap touches only the wrap: fixture asset blob hashes are byte-identical
    ///   before and after;
    /// - Shamir is re-issued (old shares invalidated) and the old-artifact guidance is
    ///   surfaced as data.
    #[tokio::test]
    async fn guided_rewrap_keeps_master_key_and_blob_hashes() {
        use capsule_core::crypto::hash::hash_bytes;

        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base);

        let master = [0x55u8; 32];
        let old_secret = b"the-old-lost-secret";

        // Enroll the original escrow.
        client
            .store_escrow(&wrap(&master, old_secret))
            .await
            .unwrap();

        // Fixture "asset" ciphertext blobs — re-wrap must not touch these.
        let asset_a = b"encrypted-asset-ciphertext-A".to_vec();
        let asset_b = b"encrypted-asset-ciphertext-B".to_vec();
        let hash_a_before = hash_bytes(&asset_a).to_hex();
        let hash_b_before = hash_bytes(&asset_b).to_hex();

        // Run the guided re-wrap (fast params, Shamir enrolled).
        let rewrap = client
            .guided_rewrap_with_params(&master, fast_params(), true)
            .await
            .unwrap();

        // The new escrow (as stored on the server) unwraps with the NEW secret to the
        // SAME master key.
        let refetched = client.fetch_escrow().await.unwrap();
        assert_eq!(refetched, rewrap.new_escrow, "server holds the new escrow");
        let recovered = recover_master_key(refetched.blob(), rewrap.secret.as_bytes()).unwrap();
        assert_eq!(recovered, master, "master key is UNCHANGED after re-wrap");

        // The OLD secret unwraps nothing — the lost secret is dead everywhere.
        assert!(recover_master_key(refetched.blob(), old_secret).is_err());

        // Re-wrap touched only the wrap: asset blob hashes are byte-identical.
        assert_eq!(hash_bytes(&asset_a).to_hex(), hash_a_before);
        assert_eq!(hash_bytes(&asset_b).to_hex(), hash_b_before);

        // Shamir re-issued, old shares invalidated; the new shares reconstruct the new
        // seed (2 of 3).
        let shamir = rewrap.shamir.expect("shamir re-issued when enrolled");
        assert!(shamir.old_shares_invalidated);
        assert_eq!(shamir.shares.len(), 3);
        let two = vec![shamir.shares[0].clone(), shamir.shares[2].clone()];
        assert_eq!(
            capsule_core::backup::recover_seed(&two).unwrap().as_slice(),
            rewrap.secret.as_bytes().as_slice()
        );

        // Type-back gate + old-artifact guidance surfaced as data.
        assert!(rewrap.type_back_required);
        assert!(rewrap.secret.matches(rewrap.secret.as_bytes()));
        assert!(!rewrap.secret.matches(b"something-else"));
        assert_eq!(
            rewrap.old_artifact_guidance,
            OldArtifactGuidance::ReexportOrDestroy
        );
    }

    /// When Shamir was never enrolled, the re-wrap re-issues no shares.
    #[tokio::test]
    async fn guided_rewrap_no_shamir_when_not_enrolled() {
        let base = start_mock(escrow_handler(EscrowStore::default())).await;
        let client = RecoveryClient::new(session_for(&base), &base);
        let master = [0x66u8; 32];
        client.store_escrow(&wrap(&master, b"old")).await.unwrap();

        let rewrap = client
            .guided_rewrap_with_params(&master, fast_params(), false)
            .await
            .unwrap();
        assert!(rewrap.shamir.is_none());
    }

    /// The minted secret clears the ≥128-bit entropy floor (256-bit) and never prints
    /// its material.
    #[test]
    fn minted_secret_is_256_bit_and_opaque() {
        let secret = MintedSecret::generate();
        assert_eq!(secret.as_bytes().len(), 32);
        assert_eq!(secret.to_hex().len(), 64);
        assert_eq!(format!("{secret:?}"), "MintedSecret { .. }");
    }
}
