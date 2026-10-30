//! Local album subscriptions: a member hands their own server the capability an album's home
//! server minted for it, and this server pulls the album with it (#1035).
//!
//! ```text
//! POST   /v1/federation/subscriptions            { issuer, token }
//! 201 | 200 { peer, album_id, scope, jti, issued_at, expires_at, subscribed_at, live }
//! 400 error.federation.capability_malformed
//! 403 error.federation.peer_unknown | error.moderation.server_blocked
//!     | error.federation.capability_invalid | error.federation.capability_expired
//! 404 error.federation.not_configured
//! 500 error.federation.unavailable
//!
//! GET    /v1/federation/subscriptions
//! 200 { subscriptions: [ … ] }
//! 404 error.federation.not_configured
//! 500 error.federation.unavailable
//!
//! DELETE /v1/federation/subscriptions/{peer}/{album_id}
//! 204 (idempotent)
//! 404 error.federation.not_configured
//! 500 error.federation.unavailable
//! ```
//!
//! # The receiving half of Token Lifecycle step 1
//!
//! design/federation.md: the owner mints on the album's home server, "the token travels in the
//! share invite to Alice's client, which posts it to `other.tld`". This is `other.tld`'s side of
//! that post. The credential is the member's own session (`Auth<AccessToken>`): the capability in
//! the body is the album's, not the caller's, and this server never presents it as the caller.
//!
//! # Verified before it is stored
//!
//! A posted token is untrusted input, so nothing about it is recorded until it verifies under
//! the **issuer's pinned key** — the key an operator pinned with `capsule-server peer pin`, the
//! same one report intake verifies against — and names this server as its `sub`. A grant for
//! another server, one signed by a key nobody pinned, or one already expired would be refused by
//! the issuer at the first pull, so it is refused here instead, where the member can be told.
//! The issuer must not be blocked: a blocked peer's jobs never run, and a subscription to one is
//! a row that can only mislead its subscriber.
//!
//! The issuer travels beside the token rather than being read out of it, so no claim is acted on
//! before the signature over it has verified. Both are bounded before anything else is looked at:
//! the issuer by [`PeerId::parse`], the token by [`MAX_CAPABILITY_BYTES`].
//!
//! # Gated like the rest of federation
//!
//! Mounted behind [`FederationGate`](super::federation::FederationGate), the first interceptor
//! of the two writes' group and of the list's own read group: a deployment that does not
//! federate answers all three `404 error.federation.not_configured` before the session is
//! authenticated, and pulls nothing.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use super::federation::{FederationTag, WireScope};
use crate::auth::AccessToken;
use crate::federation::subscriptions::MAX_CAPABILITY_BYTES;
use crate::federation::{
    CapabilityError, CapabilityToken, FederationContext, PeerId, PutOutcome, Subscription,
    verify_received,
};
use crate::store::{AlbumId, UserId};

/// The subscription request: the capability a member's client received in a share invite.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct SubscribeRequest {
    /// The album's home server, which minted the capability, as its own `server-info` names
    /// itself (`home.tld`).
    pub issuer: String,
    /// The capability, exactly as the invite carried it.
    pub token: String,
}

/// One subscription, as its subscriber sees it. Never the token.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionView {
    /// The album's home server, which every pull fetches from.
    pub peer: String,
    /// The album, as its home server names it.
    pub album_id: String,
    /// What the capability permits.
    pub scope: WireScope,
    /// The capability's `jti`.
    pub jti: String,
    /// When the capability was issued, RFC 3339.
    pub issued_at: String,
    /// When the capability stops being honoured, RFC 3339.
    pub expires_at: String,
    /// When this subscription was first recorded, RFC 3339.
    pub subscribed_at: String,
    /// Whether the capability is still honoured, so the album is still being pulled.
    ///
    /// `false` means the capability lapsed before anything renewed it; posting a fresh invite
    /// revives the subscription.
    pub live: bool,
}

impl SubscriptionView {
    fn of(subscription: &Subscription, now: jiff::Timestamp) -> Self {
        Self {
            peer: subscription.peer.as_str().to_owned(),
            album_id: subscription.album.as_str().to_owned(),
            scope: subscription.scope.into(),
            jti: subscription.jti.clone(),
            issued_at: subscription.issued_at.to_string(),
            expires_at: subscription.expires_at.to_string(),
            subscribed_at: subscription.subscribed_at.to_string(),
            live: subscription.is_live(now),
        }
    }
}

/// The caller's subscriptions.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionsResponse {
    /// Every subscription, live or lapsed, ordered by peer and then album.
    pub subscriptions: Vec<SubscriptionView>,
}

/// One subscription of the caller's.
#[derive(PathParams, Schema)]
pub struct SubscriptionPath {
    /// The album's home server.
    pub peer: String,
    /// The album, as its home server names it.
    pub album_id: String,
}

/// Why a subscription was not recorded.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum SubscribeRejection {
    /// The issuer is not a host name, or the token is not a capability in the contract's shape.
    #[error("the subscription request is malformed")]
    #[problem(status = 400, title = "Malformed request")]
    Malformed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The issuer is unknown to this server or blocked by it, or the capability did not verify,
    /// was issued to another server, or has expired. The code says which.
    #[error("this capability cannot be used here")]
    #[problem(status = 403, title = "Forbidden")]
    Refused {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was recorded.
    #[error("the subscription could not be recorded")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl SubscribeRejection {
    fn malformed() -> Self {
        Self::Malformed {
            code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
        }
    }

    fn refused(code: &'static str) -> Self {
        Self::Refused { code }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::FEDERATION_UNAVAILABLE,
        }
    }
}

/// Why the subscriptions could not be read or one removed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum SubscriptionsRejection {
    /// The store could not answer.
    #[error("the subscriptions could not be reached")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl SubscriptionsRejection {
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::FEDERATION_UNAVAILABLE,
        }
    }
}

/// The two ways a subscription is recorded.
#[derive(Reply)]
pub enum SubscribeReply {
    /// The caller held no subscription to this album; this one is recorded.
    #[reply(status = 201, description = "The subscription was recorded")]
    Created(SubscriptionView),
    /// The caller already held one. Its capability was replaced if the posted one lives
    /// longer, and kept otherwise; either way this is the subscription as it now stands.
    #[reply(
        status = 200,
        description = "The caller already held this subscription; here it is as it now stands"
    )]
    Updated(SubscriptionView),
}

/// The 403 code a capability refusal answers.
fn refusal_code(error: &CapabilityError) -> Option<&'static str> {
    match error {
        CapabilityError::Expired => Some(error_codes::FEDERATION_CAPABILITY_EXPIRED),
        CapabilityError::Unreadable
        | CapabilityError::WrongIssuer
        | CapabilityError::WrongRecipient
        | CapabilityError::NotYetValid => Some(error_codes::FEDERATION_CAPABILITY_INVALID),
        CapabilityError::Malformed { .. } | CapabilityError::BeyondTtlCeiling => None,
    }
}

/// Subscribe to a federated album with the capability its home server minted for this server.
///
/// The capability is verified against the issuer's pinned key before anything is recorded, and
/// must name this server as its recipient. Posting one for an album already subscribed keeps
/// whichever capability lives longer, so a replayed invite never undoes a renewal.
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate —
/// `FEDERATION_ENABLED` is off, which is the default — before the credential is read or a store
/// is touched.
#[kynos::post(
    "/v1/federation/subscriptions",
    operation_id = "subscribe_album",
    tag = FederationTag
)]
pub async fn subscribe_album(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
    Json(request): Json<SubscribeRequest>,
) -> Result<SubscribeReply, SubscribeRejection> {
    let subscriber = UserId::new(credential.user.as_str());
    let Ok(issuer) = PeerId::parse(&request.issuer) else {
        tracing::info!(%subscriber, length = request.issuer.len(), "a subscription named an issuer that is not a host name");
        return Err(SubscribeRejection::malformed());
    };
    if request.token.is_empty() || request.token.len() > MAX_CAPABILITY_BYTES {
        tracing::info!(%subscriber, %issuer, length = request.token.len(), "a subscription carried a token out of bounds");
        return Err(SubscribeRejection::malformed());
    }
    let here = PeerId::new(federation.codec().server_id());
    if issuer == here {
        tracing::info!(%subscriber, "a subscription named this server as the album's home");
        return Err(SubscribeRejection::malformed());
    }

    let record = federation.peers().read(&issuer).await.map_err(|error| {
        tracing::error!(%error, %issuer, "the peer store could not answer a subscription");
        SubscribeRejection::unavailable()
    })?;
    let Some(record) = record else {
        tracing::info!(%subscriber, %issuer, "a subscription named a peer this server does not know");
        return Err(SubscribeRejection::refused(
            error_codes::FEDERATION_PEER_UNKNOWN,
        ));
    };
    if record.is_blocked() {
        tracing::info!(%subscriber, %issuer, "a subscription named a blocked peer");
        return Err(SubscribeRejection::refused(
            error_codes::MODERATION_SERVER_BLOCKED,
        ));
    }
    let Some(key) = record.signing_key else {
        tracing::info!(%subscriber, %issuer, "a subscription named a peer with no pinned key");
        return Err(SubscribeRejection::refused(
            error_codes::FEDERATION_PEER_UNKNOWN,
        ));
    };

    let now = federation.clock().now();
    let grant = verify_received(&request.token, &issuer, &key, &here, now).map_err(|error| {
        tracing::info!(%subscriber, %issuer, %error, "a subscription's capability was refused");
        refusal_code(&error).map_or_else(SubscribeRejection::malformed, SubscribeRejection::refused)
    })?;

    let outcome = federation
        .subscriptions()
        .put(Subscription {
            subscriber: subscriber.clone(),
            peer: issuer.clone(),
            album: grant.album,
            scope: grant.scope,
            jti: grant.jti,
            token: CapabilityToken::new(request.token),
            issued_at: grant.issued_at,
            expires_at: grant.expires_at,
            min_protocol_version: grant.min_protocol_version,
            subscribed_at: now,
            updated_at: now,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, %issuer, "a subscription could not be recorded");
            SubscribeRejection::unavailable()
        })?;
    let view = SubscriptionView::of(outcome.subscription(), now);
    tracing::info!(
        %subscriber,
        peer = %issuer,
        album = %view.album_id,
        jti = %view.jti,
        outcome = outcome.as_str(),
        expires_at = %view.expires_at,
        "a member subscribed to a federated album"
    );
    Ok(match outcome {
        PutOutcome::Created(_) => SubscribeReply::Created(view),
        PutOutcome::Replaced(_) | PutOutcome::Kept(_) => SubscribeReply::Updated(view),
    })
}

/// List the caller's subscriptions to federated albums, live or lapsed.
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate.
#[kynos::get(
    "/v1/federation/subscriptions",
    operation_id = "list_subscriptions",
    tag = FederationTag
)]
pub async fn list_subscriptions(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
) -> Result<Json<SubscriptionsResponse>, SubscriptionsRejection> {
    let subscriber = UserId::new(credential.user.as_str());
    let held = federation
        .subscriptions()
        .of_subscriber(&subscriber)
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, "the subscriptions could not be listed");
            SubscriptionsRejection::unavailable()
        })?;
    let now = federation.clock().now();
    tracing::debug!(%subscriber, subscriptions = held.len(), "listed an account's subscriptions");
    Ok(Json(SubscriptionsResponse {
        subscriptions: held
            .iter()
            .map(|subscription| SubscriptionView::of(subscription, now))
            .collect(),
    }))
}

/// Stop pulling one federated album for the caller.
///
/// Idempotent: a subscription the caller does not hold is the same `204`. Another member's
/// subscription to the same album is untouched, and so is anything already pulled.
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate.
#[kynos::delete(
    "/v1/federation/subscriptions/{peer}/{album_id}",
    operation_id = "unsubscribe_album",
    tag = FederationTag
)]
pub async fn unsubscribe_album(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
    Path(path): Path<SubscriptionPath>,
) -> Result<NoContent, SubscriptionsRejection> {
    let subscriber = UserId::new(credential.user.as_str());
    // Folded as every stored peer is, so `Home.Test.` removes what `home.test` recorded. Not
    // parsed: a name no subscription could carry simply matches nothing.
    let peer = PeerId::new(&path.peer);
    let album = AlbumId::new(&path.album_id);
    let removed = federation
        .subscriptions()
        .remove(&subscriber, &peer, &album)
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, %peer, %album, "a subscription could not be removed");
            SubscriptionsRejection::unavailable()
        })?;
    tracing::info!(%subscriber, %peer, %album, removed, "a member unsubscribed from a federated album");
    Ok(NoContent)
}

#[cfg(test)]
mod tests;
