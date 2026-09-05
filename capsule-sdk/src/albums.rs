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

/// A failure provisioning an album.
#[derive(Debug, thiserror::Error)]
pub enum AlbumError {
    /// The HTTP request failed on the wire, or the session could not authorize it.
    #[error("album provisioning transport: {0}")]
    Transport(String),
    /// The server refused the provisioning request.
    #[error("album provisioning refused with status {status}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// The stable `error.*` code, when the server supplied one.
        code: Option<String>,
    },
    /// The response body was missing a field or otherwise unparsable.
    #[error("malformed album provisioning response: {0}")]
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
}

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

/// The `PUT /v1/albums/{album_id}/roster` request body: the signed roster as standard base64 of
/// its canonical CBOR. One field, so the bytes the owner's device signed reach the server
/// verbatim inside a JSON operation the generated client can describe.
#[derive(Debug, Clone, Serialize)]
struct RosterRequestWire {
    roster_cbor: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RosterResponseWire {
    album_id: String,
    roster_version: u64,
    amk_epoch: u64,
    member_count: u64,
    replayed: bool,
}

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
    /// Orchestration only: the roster is signed in `capsule_core::crypto::membership` by the
    /// owner's device and sent verbatim, base64-encoded, on the generated operation's JSON
    /// shape. Idempotent under `(album_id, roster_version)`: the same bytes again succeed with
    /// `replayed`. A `409` (`error.album.roster_stale`) means the server holds a roster this one
    /// does not supersede; the caller re-syncs and republishes above it.
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
        let body = RosterRequestWire {
            roster_cbor: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        let url = format!(
            "{}/{}/roster",
            self.transport.base_url,
            album_id.hyphenated()
        );
        let response = self
            .transport
            .send(|http| http.put(&url).json(&body))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let code = response
                .json::<ApiErrorWire>()
                .await
                .ok()
                .and_then(|e| e.code);
            tracing::warn!(status = status.as_u16(), ?code, "roster publish refused");
            return Err(AlbumError::Status {
                status: status.as_u16(),
                code,
            });
        }

        let wire: RosterResponseWire = response
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
        tracing::info!(
            roster_version = wire.roster_version,
            replayed = wire.replayed,
            "album roster published"
        );
        Ok(PublishedRoster {
            album_id: echoed,
            roster_version: wire.roster_version,
            amk_epoch: wire.amk_epoch,
            member_count: wire.member_count,
            replayed: wire.replayed,
        })
    }
}

#[cfg(test)]
mod tests;
