//! Album provisioning — telling the server an album exists (slice `S-C25`).
//!
//! A container album's id is **derived from the account master key**
//! ([Organization — The Default Album]), so the client knows it before the server does. This
//! module is the one call that closes that gap: `POST /v1/albums` binds the derived UUID to
//! the authenticated owner, so [invariant 6] ("album exists; the caller has write capability
//! on it") can pass for an album the client named. Every push runs it first —
//! [`crate::push::ensure_album`] is the step, this module is the wire.
//!
//! **Idempotent, and that is the whole point.** The same id arrives from every device the
//! user owns, and again after a passphrase recovery on a fresh one. Re-provisioning is a
//! success that writes nothing (`created: false`), so a client needs no
//! "have I registered this album yet?" flag — which is precisely the synced pointer the
//! master-key derivation exists to avoid. Pushing twice therefore cannot error here.
//!
//! **No name crosses the wire.** The request body carries the id and nothing else; album
//! titles live in the encrypted sidecar and the server is not entitled to them.
//!
//! [Organization — The Default Album]: https://docs/design/organization/#the-default-album
//! [invariant 6]: https://docs/design/threat-model/validation/#server-side-validation-invariants

use serde::{Deserialize, Serialize};
use tracing::instrument;
use uuid::Uuid;

pub use crate::upload::StaticToken;

// ─── Errors ───────────────────────────────────────────────────────────────────

/// A failure on the album surface: provisioning, or publishing a roster (`S-C51`).
#[derive(Debug, thiserror::Error)]
pub enum AlbumError {
    /// The HTTP request failed on the wire, or the session could not authorize it.
    #[error("album request transport: {0}")]
    Transport(String),
    /// The server refused the request.
    #[error("album request refused with status {status}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// The stable `error.*` code, when the server supplied one.
        code: Option<String>,
        /// On either roster-version refusal — the `409 error.album.roster_stale` that is behind
        /// the server, and the `400 error.album.roster_version_leap` that is too far ahead of it
        /// — the version the server holds, which is the one a caller re-signs above. Absent on
        /// every other refusal.
        current_version: Option<u64>,
    },
    /// The response body was missing a field or otherwise unparsable.
    #[error("malformed album response: {0}")]
    Malformed(String),
}

impl AlbumError {
    /// The stable `error.*` code the server attached to a refusal, when there is one.
    /// Callers switch on this, never on the bare status.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            AlbumError::Status { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

impl From<crate::auth::AuthError> for AlbumError {
    fn from(err: crate::auth::AuthError) -> Self {
        AlbumError::Transport(err.to_string())
    }
}

// ─── Authorized transport ─────────────────────────────────────────────────────

#[derive(Clone)]
enum AlbumAuth {
    /// Drive requests through the `S-D7` session (pre-flight refresh, single-flight, one
    /// `401` refresh-and-replay).
    Session(crate::auth::Session),
    /// A fixed bearer over a plain client (tests).
    Static {
        http: reqwest::Client,
        token: String,
    },
}

/// The authorized HTTP transport for the album surface: the album endpoint root (no trailing
/// slash — `POST {base}` provisions) plus the authorization seam.
#[derive(Clone)]
pub struct AlbumTransport {
    base_url: String,
    auth: AlbumAuth,
}

impl AlbumTransport {
    /// Build a transport that authorizes through an authenticated `S-D7`
    /// [`Session`](crate::auth::Session) — the sanctioned production path. `base_url` is the
    /// album endpoint root (`{origin}/v1/albums`).
    pub fn with_session(session: crate::auth::Session, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: AlbumAuth::Session(session),
        }
    }

    /// Build a transport over a fixed bearer token (tests; callers holding a live token).
    /// Same URL layout as [`Self::with_session`].
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
            auth: AlbumAuth::Static {
                http,
                token: token.0,
            },
        }
    }

    async fn send<F>(&self, build: F) -> Result<reqwest::Response, AlbumError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        match &self.auth {
            AlbumAuth::Session(session) => Ok(session.execute(build).await?),
            AlbumAuth::Static { http, token } => build(http)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| AlbumError::Transport(e.to_string())),
        }
    }

    /// A `spargen`-generated client for the API root this transport's album endpoint hangs off,
    /// carrying the same credential.
    ///
    /// The roster publish goes through this rather than through [`Self::send`]: everything that
    /// parses or serializes in this repository is generated, and `publish_album_roster` is a
    /// JSON operation the document fully describes — request body, success body, and the
    /// `409`/`400` problem shapes with their extension members. Only `provision` still hand-writes
    /// its DTOs, and only because `POST /v1/albums` predates this seam.
    ///
    /// The root is derived by trimming the endpoint's `/v1/albums` suffix, because the generated
    /// operations carry their own absolute paths while this transport is constructed with the
    /// album endpoint (`{origin}/v1/albums`) that `POST {base}` provisions against.
    fn generated(&self) -> Result<crate::rest::Client, AlbumError> {
        let root = self
            .base_url
            .strip_suffix("/v1/albums")
            .unwrap_or(&self.base_url);
        let (http, credential) = match &self.auth {
            AlbumAuth::Session(session) => {
                let session = session.clone();
                // The session's own pre-flight refresh and single-flight coalescing, consulted
                // per request; the reactive `401` replay is the caller's, below.
                let provider: crate::rest::TokenProvider = std::sync::Arc::new(move || {
                    let session = session.clone();
                    Box::pin(async move {
                        session
                            .bearer()
                            .await
                            .map_err(|error| crate::rest::AuthError::new(error.to_string()))
                    })
                });
                (
                    crate::net::http_client()
                        .map_err(|error| AlbumError::Transport(error.to_string()))?,
                    crate::rest::Credential::Provider(provider),
                )
            }
            AlbumAuth::Static { http, token } => (
                http.clone(),
                crate::rest::Credential::Bearer(token.clone().into()),
            ),
        };
        Ok(crate::rest::Client::with_client(http, root)
            .map_err(|error| AlbumError::Transport(error.to_string()))?
            .with_credential(BEARER_SCHEME, credential))
    }
}

/// The security-scheme key the document declares for the bearer JWT; the generated client
/// attaches the registered credential to every operation whose `security` names it.
const BEARER_SCHEME: &str = "bearer";

// ─── Wire DTOs (mirror the server's transport JSON) ───────────────────────────

/// The `POST /v1/albums` request body. One field, deliberately: the server's body is strict,
/// and an album *name* is not something it is entitled to.
#[derive(Debug, Clone, Serialize)]
struct ProvisionAlbumRequestWire {
    album_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ProvisionAlbumResponseWire {
    album_id: String,
    created: bool,
}

/// The one field `provision` reads off a refusal. The roster publish reads its problems through
/// the generated client's typed error instead, which is why nothing here describes extensions.
#[derive(Deserialize)]
struct ApiErrorWire {
    #[serde(default)]
    code: Option<String>,
}

/// What provisioning an album resolved to. Both cases are successes; `created` is
/// informational — a caller treats a fresh binding and an existing one identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedAlbum {
    /// The album id the server bound, echoed back.
    pub album_id: Uuid,
    /// `true` when this call created the binding, `false` when it already existed.
    pub created: bool,
}

/// What the server holds for an album after a roster publish (`S-C51`).
///
/// `replayed` is informational: the same bytes again are a success that wrote nothing, exactly
/// as re-provisioning is, so a client that lost an acknowledgement re-PUTs without branching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedRoster {
    /// The album, echoed.
    pub album_id: Uuid,
    /// The roster version the server holds after this call.
    pub roster_version: u64,
    /// The AMK epoch that roster reflects.
    pub amk_epoch: u64,
    /// How many members it names, the owner excluded.
    pub member_count: u64,
    /// Whether this call replayed the roster already held.
    pub replayed: bool,
}

// ─── Client ───────────────────────────────────────────────────────────────────

/// The album-provisioning client.
pub struct AlbumClient {
    transport: AlbumTransport,
}

impl AlbumClient {
    /// Build a client over an authorized [`AlbumTransport`].
    #[must_use]
    pub fn new(transport: AlbumTransport) -> Self {
        Self { transport }
    }

    /// Register `album_id` with the server, binding it to the authenticated caller.
    ///
    /// Idempotent: calling it again with the same id succeeds and writes nothing. The id is
    /// sent in its canonical lowercase hyphenated form, which is the only spelling the server
    /// stores, so two devices can never produce two rows for one album.
    #[instrument(skip(self), fields(album_id = %album_id))]
    pub async fn provision(&self, album_id: Uuid) -> Result<ProvisionedAlbum, AlbumError> {
        let body = ProvisionAlbumRequestWire {
            album_id: album_id.hyphenated().to_string(),
        };
        let url = self.transport.base_url.clone();
        let response = self
            .transport
            .send(|http| http.post(&url).json(&body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let code = response
                .json::<ApiErrorWire>()
                .await
                .ok()
                .and_then(|e| e.code);
            tracing::warn!(
                status = status.as_u16(),
                ?code,
                "album provisioning refused"
            );
            return Err(AlbumError::Status {
                status: status.as_u16(),
                code,
                current_version: None,
            });
        }

        let wire: ProvisionAlbumResponseWire = response
            .json()
            .await
            .map_err(|e| AlbumError::Malformed(e.to_string()))?;
        let echoed = Uuid::parse_str(&wire.album_id)
            .map_err(|e| AlbumError::Malformed(format!("response album_id: {e}")))?;
        if echoed != album_id {
            return Err(AlbumError::Malformed(format!(
                "server echoed album {echoed}, not the requested {album_id}"
            )));
        }
        tracing::info!(created = wire.created, "album provisioned");
        Ok(ProvisionedAlbum {
            album_id: echoed,
            created: wire.created,
        })
    }

    /// Publish `signed` as the roster of the album it names (`S-C51`).
    ///
    /// Orchestration only, and deliberately thin: the roster is signed in
    /// `capsule_core::crypto::membership` by one of the owner's devices, base64-encoded, and
    /// handed to the **generated** `publish_album_roster` operation, so every byte that is
    /// parsed or serialized on this path comes from the committed OpenAPI document. Idempotent
    /// under `(album_id, roster_version)`: the same bytes again succeed with `replayed`.
    ///
    /// Two refusals a caller acts on: a `409` (`error.album.roster_stale`) means the server holds
    /// a roster this one does not supersede, and a `400 error.album.roster_version_leap` means
    /// the version is too far *ahead* of the held one. Both carry
    /// [`current_version`](AlbumError::Status), and the repair for both is the same — re-sign the
    /// roster one above it.
    ///
    /// Under a session, a `401` is refreshed once and replayed, exactly as the sync feed does:
    /// the credential provider's pre-flight refresh cannot cover a token revoked mid-flight.
    ///
    /// # Errors
    ///
    /// [`AlbumError::Transport`] when the request did not complete, [`AlbumError::Status`] with
    /// the server's `error.*` code when it was refused, [`AlbumError::Malformed`] when the roster
    /// could not be encoded or the response could not be read.
    #[instrument(skip(self, signed), fields(album_id = %signed.roster.album_id, roster_version = signed.roster.roster_version))]
    pub async fn publish_roster(
        &self,
        signed: &capsule_core::crypto::membership::SignedAlbumRoster,
    ) -> Result<PublishedRoster, AlbumError> {
        use base64::Engine as _;

        let album_id = signed.roster.album_id;
        let bytes = capsule_core::cbor::to_canonical_vec(signed)
            .map_err(|e| AlbumError::Malformed(format!("roster encoding: {e}")))?;
        let body = crate::rest::types::RosterRequest {
            roster_cbor: base64::engine::general_purpose::STANDARD.encode(bytes),
        };

        let client = self.transport.generated()?;
        let wire = match publish(&client, album_id, &body).await {
            Ok(wire) => wire,
            Err(error) if is_unauthenticated(&error) => match &self.transport.auth {
                AlbumAuth::Session(session) => {
                    tracing::info!("the roster publish answered 401; refreshing once and retrying");
                    session.refresh().await?;
                    publish(&client, album_id, &body)
                        .await
                        .map_err(publish_refusal)?
                }
                // A fixed token cannot be refreshed, so retrying would ask the same question
                // twice.
                AlbumAuth::Static { .. } => return Err(publish_refusal(error)),
            },
            Err(error) => return Err(publish_refusal(error)),
        };

        let echoed = Uuid::parse_str(&wire.album_id)
            .map_err(|e| AlbumError::Malformed(format!("response album_id: {e}")))?;
        if echoed != album_id {
            return Err(AlbumError::Malformed(format!(
                "server echoed album {echoed}, not the requested {album_id}"
            )));
        }
        tracing::info!(
            roster_version = wire.roster_version,
            replayed = wire.replayed,
            "album roster published"
        );
        Ok(PublishedRoster {
            album_id: echoed,
            roster_version: counter(wire.roster_version, "roster_version")?,
            amk_epoch: counter(wire.amk_epoch, "amk_epoch")?,
            member_count: counter(wire.member_count, "member_count")?,
            replayed: wire.replayed,
        })
    }
}

/// One call of the generated roster operation.
///
/// The protocol date is a required parameter of every gated operation in the document, so the
/// generated signature asks for it; the value is this build's own, the same one the transport
/// sends as a default header.
async fn publish(
    client: &crate::rest::Client,
    album_id: Uuid,
    body: &crate::rest::types::RosterRequest,
) -> Result<
    crate::rest::types::RosterResponse,
    crate::rest::Error<crate::rest::PublishAlbumRosterError>,
> {
    Ok(client
        .publish_album_roster(
            album_id.hyphenated().to_string(),
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
            None,
            body,
        )
        .await?
        .into_inner())
}

/// Whether the refusal was the credential's.
fn is_unauthenticated(error: &crate::rest::Error<crate::rest::PublishAlbumRosterError>) -> bool {
    matches!(
        error,
        crate::rest::Error::Api(response)
            if matches!(
                response.inner(),
                crate::rest::PublishAlbumRosterError::Status401(_)
            )
    )
}

/// Map the generated operation's typed error onto this module's.
///
/// The `code` is what a caller switches on, and `current_version` is what the two version
/// refusals — the `409` that is behind and the `400` that is too far ahead — both carry so the
/// caller can re-sign one above what the server holds.
fn publish_refusal(error: crate::rest::Error<crate::rest::PublishAlbumRosterError>) -> AlbumError {
    use crate::rest::PublishAlbumRosterError as Refusal;

    let crate::rest::Error::Api(response) = error else {
        return AlbumError::Transport(error.to_string());
    };
    let status = response.status().as_u16();
    let (code, current_version) = match response.into_inner() {
        Refusal::Status400(problem) => (
            Some(problem.code.clone()),
            problem
                .current_version
                .and_then(|held| u64::try_from(held).ok()),
        ),
        Refusal::Status409(problem) => (
            Some(problem.code.clone()),
            problem
                .current_version
                .and_then(|held| u64::try_from(held).ok()),
        ),
        // The body-less refusal: a request past the transport's size backstop.
        Refusal::Status413 => (None, None),
        Refusal::Status401(problem)
        | Refusal::Status403(problem)
        | Refusal::Status404(problem)
        | Refusal::Status415(problem)
        | Refusal::Status422(problem)
        | Refusal::Status426(problem)
        | Refusal::Status500(problem) => (Some(problem.code.clone()), None),
    };
    tracing::warn!(status, ?code, ?current_version, "roster publish refused");
    AlbumError::Status {
        status,
        code,
        current_version,
    }
}

/// A counter the document types as a signed integer, as the SDK speaks it.
fn counter(value: i64, field: &str) -> Result<u64, AlbumError> {
    u64::try_from(value).map_err(|_| AlbumError::Malformed(format!("{field}: {value} is negative")))
}

#[cfg(test)]
mod tests;
