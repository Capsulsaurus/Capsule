//! [`FederationPull`] — a peer server pulling one shared album from its home server.
//!
//! # There is no federation protocol to speak
//!
//! design/federation.md introduces **no new data protocol**: a peer pulls through exactly the
//! primitives a client pulls through, `GET /v1/sync?album_id=` and `GET /v1/blob/{hash}`, with a
//! capability token in the `Authorization: Bearer` slot instead of a session access token. So
//! this module is *orchestration over generated calls* and contains no parser: the page comes
//! back through [`SyncConsumer::pull_album`], the bytes through the generated `get_blob` fed
//! into the same self-verifying [`RangedFetcher`](crate::fetch::RangedFetcher) every download
//! uses, and the lifecycle calls are the generated `refresh_capability` and `revoked_jti`.
//!
//! # The pull is gated on a fresh revocation list, and fails closed
//!
//! A capability is a bearer token: the only thing that can take one back before it expires is
//! the home server's published list at `/.well-known/capsule/revoked-jti`. So a puller does not
//! present a token it has not recently checked. [`FederationPull`] refreshes its snapshot when
//! the one it holds is older than the list's own `max_staleness_seconds`, refuses to pull when
//! the snapshot cannot be refreshed past that bound ([`FederationError::ListStale`]), and
//! refuses immediately when the token's `jti` is on the list
//! ([`FederationError::Revoked`]) — the same fail-closed rule the server-side verifier applies.
//!
//! Nothing here caches a decision it could re-derive: the snapshot is the list as published,
//! and the answer is recomputed from it on every call.
//!
//! # Refresh replaces the credential in place
//!
//! `POST /v1/federation/capabilities/refresh` presents the *previous* capability and answers the
//! successor; the predecessor is revoked as the successor is issued, so a puller that kept using
//! it would refuse itself on the next poll. [`FederationPull::refresh`] therefore swaps the held
//! token, its `jti` and both clients over together, under one lock.
//!
//! # Every presentation is signed by the puller
//!
//! A capability alone is a bearer token, so the home server admits one only with an
//! `X-Capsule-Request-Signature` made by the peer its `sub` names, over the request's method,
//! path and the token itself (design/federation.md, "Request Signatures"). The puller's
//! operational key signs through a [`RequestSigner`] it is handed rather than one this crate
//! holds: the key is the peer server's, and where it lives (memory, a file, an HSM) is the
//! server's business. Every request is signed afresh — a new nonce, the current time — retries
//! included, because the home server accepts each signature once.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use serde::Serialize;
use tracing::instrument;

use crate::fetch::{BlobSource, RangeOutcome};
use crate::sync::{SyncConsumer, SyncCursor, SyncError, SyncPage};
use crate::{net, rest};

/// The scheme key the generated client attaches a bearer under.
///
/// The same one a session token rides: the server registers one `bearer` component for both
/// token types, which is what keeps a capability presentable through the generated client at all.
const BEARER_SCHEME: &str = "bearer";

/// The domain separator a request signature's claim opens with (design/federation.md).
pub const REQUEST_CONTEXT: &str = "capsule-federation-request-v1";

/// The path the capability refresh is signed over.
const REFRESH_PATH: &str = "/v1/federation/capabilities/refresh";

/// What signs a puller's capability presentations: its operational Ed25519 key, the one the
/// home server's operator pinned for it.
pub trait RequestSigner: Send + Sync {
    /// The Ed25519 signature over `message`.
    fn sign(&self, message: &[u8]) -> [u8; 64];
}

/// The map a request signature covers, encoded as canonical CBOR.
///
/// The server holds its own copy of this shape; both pin the same vector
/// (`tests::the_signing_bytes_match_the_servers_vector`), so neither can drift alone.
#[derive(Serialize)]
struct RequestClaim<'a> {
    context: &'a str,
    method: &'a str,
    path: &'a str,
    token_sha256: String,
    signed_at: i64,
    nonce: &'a str,
}

/// The `X-Capsule-Request-Signature` for presenting `token` on `method path`, now, with a fresh
/// nonce.
///
/// # Errors
///
/// A description when the claim does not encode, which nothing a caller passes can cause.
pub(crate) fn request_signature(
    signer: &dyn RequestSigner,
    method: &str,
    path: &str,
    token: &str,
) -> Result<String, String> {
    // A UUIDv7's sixteen bytes: unique per call, and the instant it carries is already in `t`.
    let nonce = BASE64URL.encode(uuid::Uuid::now_v7().as_bytes());
    signature_header(
        signer,
        method,
        path,
        token,
        jiff::Timestamp::now().as_second(),
        &nonce,
    )
}

/// The header value for one request, at `signed_at` with `nonce`.
fn signature_header(
    signer: &dyn RequestSigner,
    method: &str,
    path: &str,
    token: &str,
    signed_at: i64,
    nonce: &str,
) -> Result<String, String> {
    let method = method.to_ascii_uppercase();
    let claim = RequestClaim {
        context: REQUEST_CONTEXT,
        method: &method,
        path,
        token_sha256: capsule_core::crypto::hash::hash_bytes(token.as_bytes()).to_hex(),
        signed_at,
        nonce,
    };
    let bytes = capsule_core::cbor::to_canonical_vec(&claim).map_err(|error| error.to_string())?;
    let signature = signer.sign(&bytes);
    Ok(format!(
        "t={signed_at}, n={nonce}, s={}",
        BASE64URL.encode(signature)
    ))
}

/// Why a federated pull could not proceed.
#[derive(Debug, thiserror::Error)]
pub enum FederationError {
    /// The capability's `jti` is on the home server's published revocation list.
    ///
    /// Terminal for this token. The peer asks the album's owner for a fresh grant, or stops.
    #[error("this capability has been revoked by its issuer")]
    Revoked,
    /// The revocation list could not be refreshed inside its own staleness bound.
    ///
    /// **Not** a reason to keep pulling: a list a peer cannot refresh is a list that may have
    /// revoked this token, and honouring the token anyway is exactly the failure the bound
    /// exists to prevent.
    #[error("the revocation list is staler than its own bound allows")]
    ListStale,
    /// The home server refused, or could not be reached.
    #[error("the home server answered {code}: {detail}")]
    Refused {
        /// The stable `error.*` code, where the answer carried one.
        code: String,
        /// The server's own description.
        detail: String,
    },
    /// The transport failed, or a URL could not be built.
    #[error("the pull could not be transported: {0}")]
    Transport(String),
    /// The feed answered, and the page did not survive validation.
    #[error(transparent)]
    Feed(#[from] SyncError),
    /// The home server will not take this peer's writes: its `426`, or a window it advertised
    /// that already refuses this build (issue #674).
    #[error(transparent)]
    UpgradeRequired(Box<crate::net::negotiation::UpgradeRequired>),
}

/// The revocation list as the home server last published it.
#[derive(Debug, Clone)]
pub struct RevocationSnapshot {
    /// Every `jti` the issuer currently refuses.
    pub revoked: Vec<String>,
    /// How long the issuer says a copy of this list may be relied on.
    pub max_staleness: Duration,
    /// When this peer fetched it, on the local monotonic clock.
    ///
    /// The *local* clock and not the list's `generated_at`: a peer deciding freshness from a
    /// timestamp the issuer wrote would be trusting the party whose revocations it is checking
    /// to be honest about their age.
    fetched_at: Instant,
}

impl RevocationSnapshot {
    /// Whether this copy is still inside the issuer's own bound at `now`.
    #[must_use]
    pub fn is_fresh(&self, now: Instant) -> bool {
        now.duration_since(self.fetched_at) <= self.max_staleness
    }

    /// Whether the issuer currently refuses `jti`.
    #[must_use]
    pub fn refuses(&self, jti: &str) -> bool {
        self.revoked.iter().any(|revoked| revoked == jti)
    }
}

/// What the puller currently holds: the credential, and the clients built over it.
struct Held {
    token: String,
    jti: String,
    sync: SyncConsumer,
    blobs: CapabilityBlobSource,
    client: rest::Client,
}

/// A peer server pulling one album it holds a capability for.
pub struct FederationPull {
    base_url: String,
    album_id: String,
    signer: Arc<dyn RequestSigner>,
    held: RwLock<Held>,
    snapshot: RwLock<Option<RevocationSnapshot>>,
}

impl FederationPull {
    /// A puller against `base_url`, presenting `token` for `album_id`, signing every request
    /// with `signer`.
    ///
    /// `jti` is the token's own identifier as the home server minted it — the key the revocation
    /// list is checked against. It is passed in rather than parsed out of the token, because
    /// this module holds no JWT parser and a client that read its own credential's claims would
    /// be trusting a value it never verified.
    ///
    /// `signer` holds the key the home server pinned for this peer; a request it did not sign
    /// is refused `401` however good the token is.
    ///
    /// # Errors
    ///
    /// [`FederationError::Transport`] when `base_url` is not a URL a client can hang paths off.
    pub fn new(
        base_url: &str,
        album_id: impl Into<String>,
        token: impl Into<String>,
        jti: impl Into<String>,
        signer: Arc<dyn RequestSigner>,
    ) -> Result<Self, FederationError> {
        let token = token.into();
        let jti = jti.into();
        Ok(Self {
            base_url: base_url.to_owned(),
            album_id: album_id.into(),
            held: RwLock::new(Held::build(base_url, token, jti, &signer)?),
            signer,
            snapshot: RwLock::new(None),
        })
    }

    /// The `jti` of the capability currently held.
    #[must_use]
    pub fn jti(&self) -> String {
        read(&self.held).jti.clone()
    }

    /// The token currently held, for a caller that persists it across restarts.
    #[must_use]
    pub fn token(&self) -> String {
        read(&self.held).token.clone()
    }

    /// Fetch the issuer's revocation list and keep it as this puller's snapshot.
    ///
    /// # Errors
    ///
    /// [`FederationError::Refused`] or [`FederationError::Transport`]; the snapshot is left
    /// as it was, and the next [`Self::admit`] will refuse once the old one goes stale.
    #[instrument(skip(self))]
    pub async fn poll_revocations(&self) -> Result<RevocationSnapshot, FederationError> {
        let client = read(&self.held).client.clone();
        let list = client
            .revoked_jti()
            .await
            .map_err(|error| {
                // Every refusal the list declares is coded, so whichever status carried it the
                // generated error has a problem body to read the code from.
                let code = match &error {
                    rest::Error::Api(response) => {
                        response.inner().body().map(|problem| problem.code.clone())
                    }
                    _ => None,
                };
                refusal("the revocation list", code, &error.to_string())
            })?
            .into_inner();
        // Fail **closed** on a bound this client cannot read: a negative or absurd
        // `max_staleness_seconds` means the snapshot is stale the instant it is taken, so the
        // next `admit` re-polls rather than honouring the list forever. `u64::MAX` here would
        // have read as fail-open the day the schema widened.
        let max_staleness =
            Duration::from_secs(u64::try_from(list.max_staleness_seconds).unwrap_or(0));
        let snapshot = RevocationSnapshot {
            revoked: list
                .revoked
                .into_iter()
                .map(|token| token.jti.clone())
                .collect(),
            max_staleness,
            fetched_at: Instant::now(),
        };
        tracing::debug!(
            revoked = snapshot.revoked.len(),
            max_staleness = ?snapshot.max_staleness,
            "refreshed the issuer's revocation list"
        );
        *write(&self.snapshot) = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Refuse unless the held capability is admissible right now.
    ///
    /// Refreshes the snapshot when the held one is past the issuer's own bound. Every pull goes
    /// through here, so a revoked grant stops the pull rather than being discovered one refusal
    /// at a time.
    ///
    /// # Errors
    ///
    /// [`FederationError::Revoked`] when the issuer refuses this `jti`;
    /// [`FederationError::ListStale`] when the list could not be refreshed inside its bound.
    pub async fn admit(&self) -> Result<(), FederationError> {
        let fresh = read(&self.snapshot)
            .as_ref()
            .filter(|snapshot| snapshot.is_fresh(Instant::now()))
            .cloned();
        let snapshot = match fresh {
            Some(snapshot) => snapshot,
            None => self.poll_revocations().await.map_err(|error| {
                tracing::warn!(%error, "the revocation list could not be refreshed; refusing to pull");
                FederationError::ListStale
            })?,
        };
        if snapshot.refuses(&read(&self.held).jti) {
            tracing::info!("the issuer has revoked this capability; the pull stops");
            return Err(FederationError::Revoked);
        }
        Ok(())
    }

    /// Pull one page of the album this capability covers.
    ///
    /// # Errors
    ///
    /// As [`Self::admit`], plus whatever the feed answered.
    #[instrument(skip(self, cursor), fields(album = %self.album_id))]
    pub async fn page(
        &self,
        cursor: &SyncCursor,
        page_size: u32,
    ) -> Result<SyncPage, FederationError> {
        self.admit().await?;
        let sync = read(&self.held).sync.clone();
        Ok(sync.pull_album(cursor, page_size, &self.album_id).await?)
    }

    /// Fetch one blob the page named, verified against its own address.
    ///
    /// The bytes are self-verifying: [`crate::fetch::fetch_blob`] hashes what arrives and
    /// refuses anything that is not the address asked for, so a home server cannot substitute
    /// content for a peer any more than it can for its own client.
    ///
    /// # Errors
    ///
    /// As [`Self::admit`], plus [`FederationError::Refused`] carrying the fetch's own reason —
    /// `error.federation.scope_insufficient` for a blob outside the grant's scope among them.
    #[instrument(skip(self), fields(album = %self.album_id))]
    pub async fn blob(&self, hash: &str, expected_len: u64) -> Result<Vec<u8>, FederationError> {
        self.admit().await?;
        let blobs = read(&self.held).blobs.clone();
        crate::fetch::fetch_blob(&blobs, hash, expected_len)
            .await
            .map_err(|error| FederationError::Refused {
                // The stable code the server sent, not a prose rendering of it: the doc above
                // promises `error.federation.scope_insufficient` here, and a caller that had to
                // string-match a message to find it would be matching on a message.
                code: error.error_code().unwrap_or_default().to_owned(),
                detail: error.to_string(),
            })
    }

    /// Exchange the held capability for its successor, and pull with that from now on.
    ///
    /// Idempotent at the server: a replay of the same predecessor answers the same successor,
    /// so a puller that crashed between the call and persisting the answer gets the same token
    /// back rather than a second grant.
    ///
    /// # Errors
    ///
    /// [`FederationError::Refused`] when the issuer will not continue the grant — a revoked
    /// predecessor, a blocked peer, a spent budget, a member no longer on the roster
    /// (`error.federation.member_not_on_roster`) — or [`FederationError::Transport`].
    #[instrument(skip(self))]
    pub async fn refresh(&self) -> Result<String, FederationError> {
        let (client, token) = {
            let held = read(&self.held);
            (held.client.clone(), held.token.clone())
        };
        let signature = request_signature(self.signer.as_ref(), "POST", REFRESH_PATH, &token)
            .map_err(FederationError::Transport)?;
        let refreshed = client
            .refresh_capability(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::RefreshCapabilityParams {
                    x_capsule_request_signature: Some(signature),
                    ..rest::RefreshCapabilityParams::default()
                },
            )
            .await
            .map_err(|error| match crate::client::upgrade_required_of(&error) {
                // The gate's `426`, or the typed transport's local one (issue #674).
                Some(refusal) => FederationError::UpgradeRequired(Box::new(refusal)),
                None => refresh_refusal(error),
            })?
            .into_inner();

        let held = Held::build(
            &self.base_url,
            refreshed.token.clone(),
            refreshed.jti.clone(),
            &self.signer,
        )?;
        // The predecessor is revoked the moment the successor is issued, so the swap has to be
        // one act: a puller holding one client on the old token and another on the new would
        // refuse itself on whichever request lost the race.
        *write(&self.held) = held;
        // The list a moment ago did not carry the predecessor; it does now. Dropped rather than
        // patched, so the next pull re-reads it from the issuer.
        *write(&self.snapshot) = None;
        tracing::info!(jti = %refreshed.jti, replayed = refreshed.replayed, "refreshed the capability");
        Ok(refreshed.token)
    }
}

/// Map a capability refresh refusal that is not an upgrade refusal.
///
/// Exhaustive over the generated enum on purpose, as [`describe`] is: every refusal the contract
/// declares carries a coded problem, and a status matched by a catch-all is a code dropped on the
/// floor — the `409 error.federation.member_not_on_roster` a peer acts on by asking the owner for
/// a fresh grant was, until #529. A status the contract adds later is a compile error here.
fn refresh_refusal(error: rest::Error<rest::RefreshCapabilityError>) -> FederationError {
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            rest::RefreshCapabilityError::Status400(problem)
            | rest::RefreshCapabilityError::Status401(problem)
            | rest::RefreshCapabilityError::Status403(problem)
            | rest::RefreshCapabilityError::Status404(problem)
            | rest::RefreshCapabilityError::Status409(problem)
            | rest::RefreshCapabilityError::Status413(problem)
            | rest::RefreshCapabilityError::Status426(problem)
            | rest::RefreshCapabilityError::Status429(problem)
            | rest::RefreshCapabilityError::Status500(problem) => FederationError::Refused {
                code: problem.code.clone(),
                detail: problem.detail.clone().unwrap_or_default(),
            },
        },
        other => FederationError::Transport(other.to_string()),
    }
}

impl std::fmt::Debug for FederationPull {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token: a `Debug` that printed a live credential is how one reaches a log.
        formatter
            .debug_struct("FederationPull")
            .field("base_url", &self.base_url)
            .field("album_id", &self.album_id)
            .field("jti", &read(&self.held).jti)
            .finish_non_exhaustive()
    }
}

impl Held {
    fn build(
        base_url: &str,
        token: String,
        jti: String,
        signer: &Arc<dyn RequestSigner>,
    ) -> Result<Self, FederationError> {
        Ok(Self {
            sync: SyncConsumer::with_signed_capability(base_url, token.clone(), signer.clone())
                .map_err(|error| FederationError::Transport(error.to_string()))?,
            blobs: CapabilityBlobSource::new(base_url, token.clone(), signer.clone())?,
            client: client_for(base_url, &token)?,
            token,
            jti,
        })
    }
}

/// A generated client for `base_url` carrying `token` under the bearer scheme.
fn client_for(base_url: &str, token: &str) -> Result<rest::Client, FederationError> {
    let http = net::http_client().map_err(|error| FederationError::Transport(error.to_string()))?;
    Ok(crate::client::negotiated_client(http, base_url)
        .map_err(FederationError::Transport)?
        .with_credential(
            BEARER_SCHEME,
            rest::Credential::Bearer(token.to_owned().into()),
        ))
}

/// A refusal from the home server, carrying the stable code when the answer had one.
fn refusal(doing: &str, code: Option<String>, detail: &str) -> FederationError {
    tracing::info!(%doing, ?code, %detail, "the home server refused a federated call");
    FederationError::Refused {
        code: code.unwrap_or_default(),
        detail: detail.to_owned(),
    }
}

/// Read a lock, recovering from a poisoned one.
fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write a lock, recovering from a poisoned one.
fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A [`BlobSource`] over the **generated** `get_blob`, presenting a capability.
///
/// Not [`HttpBlobSource`](crate::fetch::HttpBlobSource): that one is raw `reqwest` over an
/// SDK session, and a peer has no session. Everything that parses or serializes here is
/// generated — the `Range` parameter, the byte body and every declared refusal — and what is
/// hand-written is the mapping from a status to the fetcher's own outcome.
#[derive(Clone)]
pub struct CapabilityBlobSource {
    client: Arc<rest::Client>,
    token: Arc<str>,
    signer: Arc<dyn RequestSigner>,
}

impl CapabilityBlobSource {
    /// A source against `base_url`, presenting `token` and signing each range with `signer`.
    ///
    /// # Errors
    ///
    /// [`FederationError::Transport`] when `base_url` is not a URL a client can hang paths off.
    pub fn new(
        base_url: &str,
        token: impl Into<String>,
        signer: Arc<dyn RequestSigner>,
    ) -> Result<Self, FederationError> {
        let token: String = token.into();
        Ok(Self {
            client: Arc::new(client_for(base_url, &token)?),
            token: token.into(),
            signer,
        })
    }
}

impl std::fmt::Debug for CapabilityBlobSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CapabilityBlobSource")
    }
}

impl BlobSource for CapabilityBlobSource {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        // A zero-length window would be malformed; the fetcher never asks for one when bytes
        // remain, so it is read as the open-ended remainder.
        let range = match max_len {
            Some(len) if len > 0 => format!("bytes={start}-{}", start + len - 1),
            _ => format!("bytes={start}-"),
        };
        // Signed per range: the fetcher's next range is a new request, and the home server
        // accepts each signature once.
        let signature = match request_signature(
            self.signer.as_ref(),
            "GET",
            &format!("/v1/blob/{hash}"),
            &self.token,
        ) {
            Ok(signature) => signature,
            Err(error) => {
                tracing::error!(%error, "a federated blob request could not be signed");
                return RangeOutcome::Status {
                    status: 0,
                    code: None,
                };
            }
        };
        let params = rest::GetBlobParams {
            range: Some(range),
            x_capsule_request_signature: Some(signature),
            ..rest::GetBlobParams::default()
        };
        match self
            .client
            .get_blob(
                hash.to_owned(),
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                params,
            )
            .await
        {
            Ok(response) => {
                let bytes = match response.into_inner() {
                    rest::GetBlobResponse::Status200(bytes)
                    | rest::GetBlobResponse::Status206(bytes) => bytes,
                };
                RangeOutcome::Complete {
                    bytes: bytes.to_vec(),
                }
            }
            Err(rest::Error::Api(response)) => {
                let (status, code) = describe(response.into_inner());
                RangeOutcome::Status { status, code }
            }
            Err(error) => {
                tracing::debug!(%error, "a federated blob range request failed in transport");
                RangeOutcome::Status {
                    status: 0,
                    code: None,
                }
            }
        }
    }
}

/// The status and the stable code a declared blob refusal carries.
///
/// Exhaustive over the generated enum on purpose: a status the contract adds later is a compile
/// error here rather than a silent `0` the fetcher would read as a transport failure.
fn describe(error: rest::GetBlobError) -> (u16, Option<String>) {
    let coded =
        |status: u16, problem: Box<rest::types::CodedProblem>| (status, Some(problem.code.clone()));
    match error {
        rest::GetBlobError::Status304 => (304, None),
        rest::GetBlobError::Status400(problem) => coded(400, problem),
        rest::GetBlobError::Status401(problem) => coded(401, problem),
        rest::GetBlobError::Status403(problem) => coded(403, problem),
        rest::GetBlobError::Status404(problem) => coded(404, problem),
        rest::GetBlobError::Status409(problem) => coded(409, problem),
        rest::GetBlobError::Status410(problem) => coded(410, problem),
        rest::GetBlobError::Status412 => (412, None),
        rest::GetBlobError::Status413(problem) => coded(413, problem),
        rest::GetBlobError::Status416(problem) => coded(416, problem),
        rest::GetBlobError::Status429(problem) => coded(429, problem),
        rest::GetBlobError::Status500(problem) => coded(500, problem),
    }
}

#[cfg(test)]
mod tests;
