//! `capsule federation subscribe|list|unsubscribe` — this account's subscriptions to albums
//! whose home is another server (design/federation.md, Token Lifecycle step 1; #1117).
//!
//! A member who is shared into an album on another server receives, in the share invite, a
//! capability that album's home server minted for **this** account's home server. Posting it
//! here is what makes this server pull the album. Each command is one generated call over the
//! signed-in session; nothing is parsed by hand and nothing is stored locally.
//!
//! - `subscribe` posts the capability (`POST /v1/federation/subscriptions`). The token is a
//!   bearer credential on its issuer, so it is read from stdin, never from the command line,
//!   where shell history and the process list would keep it. Posting one for an album already
//!   subscribed renews it; the server keeps whichever token lives longer.
//! - `list` reads every subscription, live or lapsed (`GET /v1/federation/subscriptions`).
//! - `unsubscribe` stops pulling one album (`DELETE /v1/federation/subscriptions/{peer}/
//!   {album_id}`). Idempotent: an album not subscribed is the same success.
//!
//! A refusal carries the server's stable `error.*` code, rendered through the English catalog.

use capsule_core::crypto::primitives::PROTOCOL_VERSION;
use capsule_sdk::auth::Session;
use capsule_sdk::client::{AuthenticatedClient, upgrade_required_of};
use capsule_sdk::rest;
use thiserror::Error;
use tracing::instrument;

use crate::i18n::error_message;
use crate::remote::{self, RemoteConfig, RemoteError};
use crate::session::SessionStore;

/// One subscription, as `list` and `subscribe` report it. Never the token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubscriptionEntry {
    /// The album's home server, which every pull fetches from.
    pub peer: String,
    /// The album, as its home server names it.
    pub album_id: String,
    /// What the capability permits: `read` or `read-derivative-only`.
    pub scope: String,
    /// The capability's `jti`.
    pub jti: String,
    /// When the capability was issued, RFC 3339.
    pub issued_at: String,
    /// When the capability stops being honoured, RFC 3339.
    pub expires_at: String,
    /// When this subscription was first recorded, RFC 3339.
    pub subscribed_at: String,
    /// Whether the album is still being pulled. `false` means the capability lapsed; posting a
    /// fresh invite revives it.
    pub live: bool,
}

impl From<rest::types::SubscriptionView> for SubscriptionEntry {
    fn from(view: rest::types::SubscriptionView) -> Self {
        Self {
            peer: view.peer,
            album_id: view.album_id,
            scope: view.scope.to_string(),
            jti: view.jti,
            issued_at: view.issued_at,
            expires_at: view.expires_at,
            subscribed_at: view.subscribed_at,
            live: view.live,
        }
    }
}

/// What `subscribe` did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Subscribed {
    /// `true` when the account held no subscription to this album and one is now recorded;
    /// `false` when it already held one, which the posted capability renewed or left as it was.
    pub created: bool,
    /// The subscription as it now stands.
    pub subscription: SubscriptionEntry,
}

/// Why a subscription command did not complete.
#[derive(Debug, Error)]
pub enum FederationCommandError {
    /// The issuer or the token was empty before any request was made.
    #[error("{0}")]
    Invalid(&'static str),
    /// No session, or the session could not be resumed.
    #[error(transparent)]
    Remote(#[from] RemoteError),
    /// The server refused, with the status and the stable code it answered.
    #[error("the server refused ({status}{})", code.as_deref().map(|code| format!(", {code}")).unwrap_or_default())]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The `error.*` code, where the refusal carried one.
        code: Option<String>,
    },
    /// The server requires a newer client than this one.
    #[error("{0}")]
    UpgradeRequired(String),
    /// The server could not be reached, or answered something the contract does not declare.
    #[error("the server could not be reached: {0}")]
    Transport(String),
}

impl FederationCommandError {
    /// The stable `error.*` code, where the server answered one.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Refused { code, .. } => code.as_deref(),
            Self::Remote(remote) => remote.error_code(),
            Self::Invalid(_) | Self::UpgradeRequired(_) | Self::Transport(_) => None,
        }
    }
}

/// The line a failed subscription command exits with: the English catalog message for the
/// server's code where it answered one, else the error itself.
pub fn describe_error(error: &FederationCommandError) -> String {
    match error.error_code() {
        Some(code) => error_message(code),
        None => error.to_string(),
    }
}

/// `capsule federation subscribe`: post the capability `token` that `issuer` minted for this
/// account's server.
///
/// # Errors
///
/// [`FederationCommandError::Invalid`] for an empty issuer or token, before any request; else
/// what the session or the server answered.
#[instrument(skip(store, token))]
pub async fn subscribe(
    remote: &RemoteConfig,
    store: &SessionStore,
    issuer: &str,
    token: &str,
) -> Result<Subscribed, FederationCommandError> {
    let issuer = issuer.trim();
    let token = token.trim();
    if issuer.is_empty() {
        return Err(FederationCommandError::Invalid("the issuer is empty"));
    }
    if token.is_empty() {
        return Err(FederationCommandError::Invalid(
            "the capability token is empty",
        ));
    }
    let session = remote::resume_session(remote, store).await?;
    let outcome = post_subscription(&remote.sync_endpoint, session.clone(), issuer, token).await;
    remote::checkpoint(store, &session).await;
    let subscribed = outcome?;
    tracing::info!(
        peer = %subscribed.subscription.peer,
        album = %subscribed.subscription.album_id,
        created = subscribed.created,
        "subscribed to a federated album"
    );
    Ok(subscribed)
}

/// `capsule federation list`: every subscription of the signed-in account, live or lapsed,
/// ordered by peer and then album.
///
/// # Errors
///
/// What the session or the server answered.
#[instrument(skip(store))]
pub async fn list(
    remote: &RemoteConfig,
    store: &SessionStore,
) -> Result<Vec<SubscriptionEntry>, FederationCommandError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = read_subscriptions(&remote.sync_endpoint, session.clone()).await;
    remote::checkpoint(store, &session).await;
    let subscriptions = outcome?;
    tracing::info!(
        subscriptions = subscriptions.len(),
        "listed the federated subscriptions"
    );
    Ok(subscriptions)
}

/// `capsule federation unsubscribe`: stop pulling `album_id` from `peer` for this account.
///
/// # Errors
///
/// [`FederationCommandError::Invalid`] for an empty peer or album, before any request; else
/// what the session or the server answered.
#[instrument(skip(store))]
pub async fn unsubscribe(
    remote: &RemoteConfig,
    store: &SessionStore,
    peer: &str,
    album_id: &str,
) -> Result<(), FederationCommandError> {
    if peer.trim().is_empty() {
        return Err(FederationCommandError::Invalid("the peer is empty"));
    }
    if album_id.trim().is_empty() {
        return Err(FederationCommandError::Invalid("the album id is empty"));
    }
    let session = remote::resume_session(remote, store).await?;
    let outcome = delete_subscription(&remote.sync_endpoint, session.clone(), peer, album_id).await;
    remote::checkpoint(store, &session).await;
    outcome?;
    tracing::info!(%peer, %album_id, "unsubscribed from a federated album");
    Ok(())
}

/// The authenticated client over `session`, against the API root.
fn client(api_root: &str, session: Session) -> Result<AuthenticatedClient, FederationCommandError> {
    AuthenticatedClient::new(api_root, session)
        .map_err(|error| FederationCommandError::Transport(error.to_string()))
}

/// The post itself, over a session somebody else persists.
async fn post_subscription(
    api_root: &str,
    session: Session,
    issuer: &str,
    token: &str,
) -> Result<Subscribed, FederationCommandError> {
    let body = rest::types::SubscribeRequest {
        issuer: issuer.to_owned(),
        token: token.to_owned(),
    };
    let response = client(api_root, session)?
        .subscribe_album(PROTOCOL_VERSION, None, &body)
        .await
        .map_err(|error| refusal(error, subscribe_code))?;
    Ok(match response.into_inner() {
        rest::SubscribeAlbumResponse::Status201(view) => Subscribed {
            created: true,
            subscription: (*view).into(),
        },
        rest::SubscribeAlbumResponse::Status200(view) => Subscribed {
            created: false,
            subscription: (*view).into(),
        },
    })
}

/// The listing itself, over a session somebody else persists.
async fn read_subscriptions(
    api_root: &str,
    session: Session,
) -> Result<Vec<SubscriptionEntry>, FederationCommandError> {
    let response = client(api_root, session)?
        .list_subscriptions(PROTOCOL_VERSION, None)
        .await
        .map_err(|error| refusal(error, list_code))?;
    Ok(response
        .into_inner()
        .subscriptions
        .into_iter()
        .map(SubscriptionEntry::from)
        .collect())
}

/// The removal itself, over a session somebody else persists.
async fn delete_subscription(
    api_root: &str,
    session: Session,
    peer: &str,
    album_id: &str,
) -> Result<(), FederationCommandError> {
    client(api_root, session)?
        .unsubscribe_album(peer.to_owned(), album_id.to_owned(), PROTOCOL_VERSION, None)
        .await
        .map_err(|error| refusal(error, unsubscribe_code))?;
    Ok(())
}

/// Map a generated-client failure: a declared refusal to its status and code, an upgrade
/// demand to its own code, everything else to a transport failure.
fn refusal<E>(error: rest::Error<E>, code_of: fn(&E) -> String) -> FederationCommandError
where
    E: std::error::Error + 'static,
{
    if let Some(upgrade) = upgrade_required_of(&error) {
        tracing::warn!(%upgrade, "a subscription request was refused: this build is too old");
        return FederationCommandError::UpgradeRequired(upgrade.to_string());
    }
    match error {
        rest::Error::Api(response) => {
            let status = response.status().as_u16();
            let code = code_of(&response.into_inner());
            tracing::warn!(status, %code, "a subscription request was refused");
            FederationCommandError::Refused {
                status,
                code: Some(code),
            }
        }
        rest::Error::UnexpectedStatus { status, .. } => FederationCommandError::Refused {
            status: status.as_u16(),
            code: None,
        },
        other => FederationCommandError::Transport(other.to_string()),
    }
}

fn subscribe_code(error: &rest::SubscribeAlbumError) -> String {
    use rest::SubscribeAlbumError as Refusal;
    match error {
        Refusal::Status400(problem)
        | Refusal::Status401(problem)
        | Refusal::Status403(problem)
        | Refusal::Status404(problem)
        | Refusal::Status413(problem)
        | Refusal::Status415(problem)
        | Refusal::Status422(problem)
        | Refusal::Status425(problem)
        | Refusal::Status426(problem)
        | Refusal::Status500(problem) => problem.code.clone(),
    }
}

fn list_code(error: &rest::ListSubscriptionsError) -> String {
    use rest::ListSubscriptionsError as Refusal;
    match error {
        Refusal::Status400(problem)
        | Refusal::Status401(problem)
        | Refusal::Status403(problem)
        | Refusal::Status404(problem)
        | Refusal::Status413(problem)
        | Refusal::Status500(problem) => problem.code.clone(),
    }
}

fn unsubscribe_code(error: &rest::UnsubscribeAlbumError) -> String {
    use rest::UnsubscribeAlbumError as Refusal;
    match error {
        Refusal::Status400(problem)
        | Refusal::Status401(problem)
        | Refusal::Status403(problem)
        | Refusal::Status404(problem)
        | Refusal::Status413(problem)
        | Refusal::Status425(problem)
        | Refusal::Status426(problem)
        | Refusal::Status500(problem) => problem.code.clone(),
    }
}

#[cfg(test)]
mod tests;
