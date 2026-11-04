//! Local album subscriptions: a member hands their own server the capability an album's home
//! server minted for it, and this server pulls the album with it (#1035).
//!
//! ```text
//! POST   /v1/federation/subscriptions            { issuer, token }
//! 201 | 200 { peer, album_id, scope, jti, issued_at, expires_at, subscribed_at, live }
//! 400 error.federation.capability_malformed
//! 403 error.federation.peer_unknown | error.moderation.server_blocked
//!     | error.federation.capability_invalid | error.federation.capability_expired
//!     | error.federation.subscription_limit
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
//!
//! GET    /v1/federation/subscriptions/{peer}/{album_id}/assets?after=&limit=
//! 200 { peer, album_id, assets: [ { asset_id, head, timestamp, author, size } ], next_after }
//! 404 error.federation.not_configured | error.federation.album_not_found
//! 500 error.federation.unavailable
//!
//! GET    /v1/federation/subscriptions/{peer}/{album_id}/assets/{asset_id}/metadata
//! 200 application/octet-stream
//! 404 error.federation.not_configured | error.federation.album_not_found
//! 500 error.federation.unavailable
//! ```
//!
//! # What a subscriber reads of a pulled album
//!
//! The two `assets` reads are how a member's client renders an album its server pulled for it:
//! its assets as their breadcrumbs, and each asset's encrypted metadata blob, both read through
//! [`PulledAlbumReads`](crate::federation::pull::pulled::PulledAlbumReads) from what a pull
//! committed and nothing else. Only a subscriber reads: an album the caller holds no
//! subscription to answers `404 error.federation.album_not_found` whether or not another member
//! subscribed to it and whatever was pulled for them, so the reads cannot be used to learn
//! another account's subscriptions. An asset with no metadata to serve is the same answer.
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
//! # Bounded per account
//!
//! An account holds at most [`MAX_SUBSCRIPTIONS_PER_ACCOUNT`] subscriptions, live or lapsed. A
//! post that would create one more is refused `403 error.federation.subscription_limit` after
//! the capability has verified, so the code is only ever news about the account, never about a
//! token. Waiting does not clear it; unsubscribing does. A post for an album already subscribed
//! is a renewal and is never refused.
//!
//! # Gated like the rest of federation
//!
//! Mounted behind [`FederationGate`](super::federation::FederationGate), the first interceptor
//! of the two writes' group and of the reads' own group: a deployment that does not federate
//! answers all five `404 error.federation.not_configured` before the session is authenticated,
//! and pulls nothing.

use capsule_i18n::error_codes;
use kynos::extract::body::binary::Binary;
use kynos::http::media::OctetStream;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use super::federation::{FederationTag, WireScope};
use crate::auth::AccessToken;
use crate::federation::breadcrumbs::{Breadcrumb, MAX_BREADCRUMB_PAGE};
use crate::federation::pull::JobKey;
use crate::federation::subscriptions::{MAX_CAPABILITY_BYTES, MAX_SUBSCRIPTIONS_PER_ACCOUNT};
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

    /// The issuer is unknown to this server or blocked by it, the capability did not verify,
    /// was issued to another server, or has expired, or the caller already holds as many
    /// subscriptions as an account may. The code says which.
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
/// whichever capability lives longer, so a replayed invite never undoes a renewal. An account
/// holds at most 1,000 subscriptions; one more is refused `403
/// error.federation.subscription_limit` until the caller unsubscribes from another.
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
        .put_within(
            Subscription {
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
            },
            MAX_SUBSCRIPTIONS_PER_ACCOUNT,
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, %issuer, "a subscription could not be recorded");
            SubscribeRejection::unavailable()
        })?;
    let Some(outcome) = outcome else {
        tracing::info!(
            %subscriber,
            %issuer,
            limit = MAX_SUBSCRIPTIONS_PER_ACCOUNT,
            "a subscription was refused: the account holds as many as it may"
        );
        return Err(SubscribeRejection::refused(
            error_codes::FEDERATION_SUBSCRIPTION_LIMIT,
        ));
    };
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

/// How many assets a page of a pulled album answers when the caller names no `limit`.
pub const DEFAULT_PULLED_ASSETS_PAGE: u32 = 100;

/// The page a pulled album's assets are asked for in.
#[derive(Schema, QueryParams, Debug)]
pub struct PulledAssetsQuery {
    /// Resume after this asset id: the `next_after` the previous page answered. Absent starts
    /// from the first asset.
    pub after: Option<String>,
    /// How many assets to answer: 100 when absent, and clamped into 1 to 1,000.
    pub limit: Option<u32>,
}

/// One asset of a pulled album: its breadcrumb, built from the latest entry this server's pull
/// accepted for it.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PulledAssetView {
    /// The asset, as its home server names it.
    pub asset_id: String,
    /// Its chain head: the hash of its latest accepted provenance record, lowercase hex.
    pub head: String,
    /// When the head record says it was written, RFC 3339. Self-asserted by its author.
    pub timestamp: String,
    /// The account that wrote the head record, on the album's home server.
    pub author: String,
    /// The asset's plaintext size in bytes.
    pub size: u64,
}

impl PulledAssetView {
    fn of(breadcrumb: &Breadcrumb) -> Self {
        Self {
            asset_id: breadcrumb.asset_id.clone(),
            head: breadcrumb.hash.to_hex(),
            timestamp: breadcrumb.timestamp.to_string(),
            author: breadcrumb.author.to_string(),
            size: breadcrumb.size,
        }
    }
}

/// A page of the assets this server pulled for one of the caller's subscriptions.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PulledAssetsResponse {
    /// The album's home server.
    pub peer: String,
    /// The album, as its home server names it.
    pub album_id: String,
    /// The album's live assets after `after`, by asset id in byte order. Deleted and purged
    /// assets are not listed.
    pub assets: Vec<PulledAssetView>,
    /// The `after` that asks for the next page; absent once a page came back short, so the
    /// album has been listed to its end.
    pub next_after: Option<String>,
}

/// One of the caller's pulled albums.
#[derive(PathParams, Schema)]
pub struct PulledAlbumPath {
    /// The album's home server.
    pub peer: String,
    /// The album, as its home server names it.
    pub album_id: String,
}

/// One asset of one of the caller's pulled albums.
#[derive(PathParams, Schema)]
pub struct PulledAssetPath {
    /// The album's home server.
    pub peer: String,
    /// The album, as its home server names it.
    pub album_id: String,
    /// The asset, as its home server names it.
    pub asset_id: String,
}

/// Why a pulled album could not be read.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum PulledAlbumRejection {
    /// The caller holds no subscription to this album, or, for one asset, the album holds no
    /// live asset by that id with a metadata blob. One answer for every case, so the route
    /// never tells a caller what another member's subscriptions pulled.
    #[error("no such album")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the pulled album could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl PulledAlbumRejection {
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::FEDERATION_ALBUM_NOT_FOUND,
        }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::FEDERATION_UNAVAILABLE,
        }
    }
}

/// The job behind `subscriber`'s subscription to `album_id` on `peer`, or the one not-found
/// answer when the subscriber holds none, live or lapsed.
///
/// A lapsed subscription still reads: what its pulls committed is still held, and still
/// charged to the subscriber, until it unsubscribes.
async fn subscribed_job(
    federation: &FederationContext,
    subscriber: &UserId,
    peer: &str,
    album_id: &str,
) -> Result<JobKey, PulledAlbumRejection> {
    // Folded as every stored peer is, and not parsed, as on unsubscribe: a name no
    // subscription could carry simply matches nothing.
    let peer = PeerId::new(peer);
    let album = AlbumId::new(album_id);
    let held = federation
        .subscriptions()
        .of_subscriber(subscriber)
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, %peer, %album, "the subscriptions could not be read for a pulled album");
            PulledAlbumRejection::unavailable()
        })?;
    if !held
        .iter()
        .any(|subscription| subscription.peer == peer && subscription.album == album)
    {
        tracing::info!(%subscriber, %peer, %album, "a pulled album was asked for by an account not subscribed to it");
        return Err(PulledAlbumRejection::not_found());
    }
    Ok(JobKey::new(peer, album))
}

/// List the assets this server pulled for one of the caller's subscriptions, a page at a time.
///
/// Each asset is its breadcrumb, read from the latest entry a pull accepted for it, so nothing
/// listed is anything but what passed this server's re-validation. Deleted and purged assets
/// are not listed. A subscription nothing has been pulled for yet answers an empty page.
///
/// Answers `404 error.federation.album_not_found` when the caller holds no subscription to the
/// album, live or lapsed, whoever else does; and `404 error.federation.not_configured` when
/// this deployment does not federate.
#[kynos::get(
    "/v1/federation/subscriptions/{peer}/{album_id}/assets",
    operation_id = "list_pulled_assets",
    tag = FederationTag
)]
pub async fn list_pulled_assets(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
    Path(path): Path<PulledAlbumPath>,
    Query(query): Query<PulledAssetsQuery>,
) -> Result<Json<PulledAssetsResponse>, PulledAlbumRejection> {
    let subscriber = UserId::new(credential.user.as_str());
    let job = subscribed_job(&federation, &subscriber, &path.peer, &path.album_id).await?;
    let limit = usize::try_from(query.limit.unwrap_or(DEFAULT_PULLED_ASSETS_PAGE))
        .unwrap_or(MAX_BREADCRUMB_PAGE)
        .clamp(1, MAX_BREADCRUMB_PAGE);
    let breadcrumbs = federation
        .pulled()
        .breadcrumbs(&job, query.after.as_deref(), limit)
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, %job, "a pulled album's breadcrumbs could not be read");
            PulledAlbumRejection::unavailable()
        })?;
    let next_after = (breadcrumbs.len() == limit)
        .then(|| breadcrumbs.last().map(|crumb| crumb.asset_id.clone()))
        .flatten();
    tracing::debug!(%subscriber, %job, assets = breadcrumbs.len(), more = next_after.is_some(), "listed a pulled album's assets");
    Ok(Json(PulledAssetsResponse {
        peer: job.peer.as_str().to_owned(),
        album_id: job.album.as_str().to_owned(),
        assets: breadcrumbs.iter().map(PulledAssetView::of).collect(),
        next_after,
    }))
}

/// Fetch the encrypted metadata blob one asset of a pulled album carries, as the pull stored it.
///
/// Opaque octets, byte for byte what the album's home server served and this server verified
/// against its address: this server holds no key to them. The blob is the one the asset's
/// latest accepted entry carries.
///
/// Answers `404 error.federation.album_not_found` when the caller holds no subscription to the
/// album, or the album holds no live asset by that id with a metadata blob; and `404
/// error.federation.not_configured` when this deployment does not federate.
#[kynos::get(
    "/v1/federation/subscriptions/{peer}/{album_id}/assets/{asset_id}/metadata",
    operation_id = "get_pulled_asset_metadata",
    tag = FederationTag
)]
pub async fn get_pulled_asset_metadata(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
    Path(path): Path<PulledAssetPath>,
) -> Result<Binary<OctetStream>, PulledAlbumRejection> {
    let subscriber = UserId::new(credential.user.as_str());
    let job = subscribed_job(&federation, &subscriber, &path.peer, &path.album_id).await?;
    let bytes = federation
        .pulled()
        .asset_metadata(&job, &path.asset_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %subscriber, %job, asset = %path.asset_id, "a pulled asset's metadata could not be read");
            PulledAlbumRejection::unavailable()
        })?;
    let Some(bytes) = bytes else {
        tracing::info!(%subscriber, %job, asset = %path.asset_id, "a pulled asset has no metadata to serve");
        return Err(PulledAlbumRejection::not_found());
    };
    tracing::debug!(%subscriber, %job, asset = %path.asset_id, bytes = bytes.len(), "served a pulled asset's metadata");
    Ok(Binary::new(bytes))
}

#[cfg(test)]
mod tests;
