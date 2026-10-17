//! The `.well-known/capsule/*` registry — the attestation-key record (slice `S-C15`).
//!
//! # Public, and that is the point
//!
//! This is the only operation on the server with no `Auth`. A client pins these keys on first
//! contact and verifies receipts against them for the life of the assets they cover; a peer
//! checking a proof of loss has no account here at all. Requiring a credential to fetch the key
//! that checks the server's own liability would let the server decline to be checked.
//!
//! Nothing here is user-scoped. The registry's rule is explicit — *never a user list* — and this
//! record is server-scoped by construction: it carries an origin and public keys.
//!
//! # The history is append-only, and why that is the whole record
//!
//! A receipt names the key that signed it (`server_key_id`), so a key retired years ago must
//! still resolve or every receipt under it becomes unverifiable at once — which from outside is
//! indistinguishable from the server having forged them. Publishing only the *active* key would
//! make rotation a silent repudiation of everything signed before it.
//!
//! [`crate::attestation`] owns the history and derives the active key's entry from the signer,
//! so a server cannot publish a set that omits the key it is currently signing with.
//!
//! # The other registry records (`S-C18`)
//!
//! `server-info`, `revoked-jti` and `deprecation` are served here too, from
//! [`crate::discovery`], and every one of them is public for the same reason: a client deciding
//! whether it can talk to this server at all, and a peer checking whether a capability token it
//! holds is still good, are both by construction unauthenticated here.
//!
//! `moved/{user}` is post-v1 with Account Portability and is deliberately not served — it is
//! the one record that would name a user, admissible only because the user signs it and the
//! user initiates the migration. Adding it is a decision about that exception, not another row.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attestation::AttestationContext;
use crate::discovery::revocation::MAX_STALENESS;
use crate::discovery::{DeprecationAnnouncement, DiscoveryContext};

/// The discovery surface: what a client or peer can learn without an account.
#[derive(Tag)]
#[tag(
    name = "well-known",
    description = "Public, server-scoped discovery records. Never a user list."
)]
pub struct WellKnownTag;

/// One published attestation key.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PublishedKeyResponse {
    /// The fingerprint a receipt's `server_key_id` selects on, lowercase hex.
    pub key_id: String,
    /// The hybrid public key, base64 (Ed25519 ‖ ML-DSA-65).
    pub public: String,
    /// The signature algorithm this key is used with.
    pub algorithm: String,
    /// When it began signing, RFC 3339.
    pub active_from: String,
    /// When it stopped, or absent while it is the active key.
    pub active_to: Option<String>,
}

/// The `.well-known/capsule/attestation-keys` record.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AttestationKeysResponse {
    /// This server's canonical origin — the other half of the binding that refuses a
    /// cross-server replay.
    pub server_id: String,
    /// Every key this server has signed with, oldest first, the active one last.
    pub keys: Vec<PublishedKeyResponse>,
}

/// The algorithm identifier the hybrid attestation signature is published under.
const ALGORITHM: &str = "hybrid-ed25519-mldsa65";

/// Serve this server's storage-attestation keys and their append-only history.
///
/// Cacheable and unauthenticated. It changes only when a key rotates, and a client that pinned
/// a stale copy still resolves every receipt signed before it fetched — which is the property
/// the append-only ordering buys.
#[kynos::get(
    "/.well-known/capsule/attestation-keys",
    operation_id = "attestation_keys",
    tag = WellKnownTag
)]
pub async fn attestation_keys(
    Inject(attestation): Inject<AttestationContext>,
) -> Json<AttestationKeysResponse> {
    Json(AttestationKeysResponse {
        server_id: attestation.signer().server_id().to_owned(),
        keys: attestation
            .history()
            .iter()
            .map(|key| PublishedKeyResponse {
                key_id: key.key_id.to_hex(),
                public: BASE64.encode(key.public.to_bytes()),
                algorithm: ALGORITHM.to_owned(),
                active_from: key.active_from.to_string(),
                active_to: key.active_to.map(|at| at.to_string()),
            })
            .collect(),
    })
}

/// The `.well-known/capsule/server-info` record.
///
/// Server-scoped facts only. The registry's rule — *never a user list* — is structural here:
/// this type holds no user-shaped field, so there is nothing for a future edit to leak through.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ServerInfoResponse {
    /// This server's canonical origin.
    pub server_id: String,
    /// Where the versioned API lives.
    pub api_base_url: String,
    /// Where a client performs the auth ceremony.
    pub auth: AuthEndpointsResponse,
    /// Where federated peers talk to this server. Absent when it does not federate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub federation_url: Option<String>,
    /// The `protocol_version` range accepted for writes today, both ends inclusive.
    pub protocol_version: ProtocolWindowResponse,
    /// The raw Ed25519 public key this server's tokens verify under, base64.
    pub signing_key: String,
    /// The signature algorithm that key is used with.
    pub signing_algorithm: String,
    /// Announced deprecation cutoffs, in announcement order. Empty when none is pending.
    pub deprecations: Vec<DeprecationResponse>,
}

/// The auth ceremony's endpoints.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AuthEndpointsResponse {
    /// Where a session is opened.
    pub login: String,
    /// Where an access token is rotated.
    pub refresh: String,
    /// Where a session is ended.
    pub logout: String,
    /// Where a sign-in through an external identity provider begins and ends, or `null` when
    /// this deployment has none. Always present, so a client reads one field rather than
    /// probing for one.
    pub oidc: Option<OidcEndpointsResponse>,
}

/// The OIDC ceremony's endpoints (slice `S-N1`).
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OidcEndpointsResponse {
    /// Where a client asks for an authorization URL.
    pub authorize: String,
    /// Where a client presents the `state` and `code` the provider's redirect carried.
    pub callback: String,
}

/// The accepted `protocol_version` range.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProtocolWindowResponse {
    /// The oldest version still accepted for writes.
    pub min: String,
    /// The newest version this server speaks.
    pub max: String,
}

/// One announced deprecation cutoff.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DeprecationResponse {
    /// The lowest `protocol_version` that remains accepted after the cutoff.
    pub min_protocol_version: String,
    /// When the announcement was first published, RFC 3339.
    pub announced_at: String,
    /// When versions below `min_protocol_version` stop being accepted, RFC 3339.
    pub cutoff: String,
    /// Where a human reads what to do about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_url: Option<String>,
}

/// The `.well-known/capsule/deprecation` record.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DeprecationsResponse {
    /// Every announced cutoff, in announcement order.
    pub announcements: Vec<DeprecationResponse>,
}

/// The `.well-known/capsule/revoked-jti` record.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RevokedJtiResponse {
    /// When this snapshot was taken, RFC 3339.
    ///
    /// Part of the record rather than left to an HTTP `Date`, because the staleness rule a peer
    /// applies is a property of the list's content — a verifier reasoning from a transport
    /// header would be trusting a cache to be honest about its own age.
    pub generated_at: String,
    /// How stale a cached copy of this list may be before it stops being usable, in seconds.
    ///
    /// Published so the rule is discoverable rather than a constant every peer implementation
    /// has to have read the same document to know.
    pub max_staleness_seconds: u32,
    /// Every revoked `jti` not yet past its own expiry, soonest expiry first.
    pub revoked: Vec<RevokedTokenResponse>,
}

/// One revoked capability token.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RevokedTokenResponse {
    /// The token's `jti` claim.
    pub jti: String,
    /// The token's own `exp`, RFC 3339. After this the entry is pruned.
    pub expires_at: String,
}

/// The algorithm the server's operational signature is published under.
///
/// Classical Ed25519, not the hybrid the attestation key uses — the operational-signature
/// carve-out in design/cryptography/primitives.md, and what a federated peer verifies a
/// capability token against offline.
const SIGNING_ALGORITHM: &str = "ed25519";

/// Serve this server's public, server-scoped facts.
///
/// Unauthenticated by contract: a client deciding whether it can talk to this server at all has
/// no credential yet, and a peer resolving the key that verifies a capability token must not
/// need one from the server whose claims it is checking.
#[kynos::get(
    "/.well-known/capsule/server-info",
    operation_id = "server_info",
    tag = WellKnownTag
)]
pub async fn server_info(Inject(discovery): Inject<DiscoveryContext>) -> Json<ServerInfoResponse> {
    let info = discovery.info();
    Json(ServerInfoResponse {
        server_id: info.server_id().to_owned(),
        api_base_url: info.api_base_url().to_owned(),
        auth: AuthEndpointsResponse {
            login: info.auth().login.clone(),
            refresh: info.auth().refresh.clone(),
            logout: info.auth().logout.clone(),
            oidc: info
                .auth()
                .oidc
                .as_ref()
                .map(|endpoints| OidcEndpointsResponse {
                    authorize: endpoints.authorize.clone(),
                    callback: endpoints.callback.clone(),
                }),
        },
        federation_url: info.federation_url().map(ToOwned::to_owned),
        protocol_version: ProtocolWindowResponse {
            min: info.protocol().min.clone(),
            max: info.protocol().max.clone(),
        },
        signing_key: BASE64.encode(info.signing_key()),
        signing_algorithm: SIGNING_ALGORITHM.to_owned(),
        deprecations: info.deprecations().iter().map(announcement).collect(),
    })
}

/// Serve the announced deprecation cutoffs.
///
/// The same announcements `server-info` carries, at their own path because that is the URL the
/// `Warning:` header on a below-cutoff response points a human at, and because a client polling
/// for a cutoff should not have to refetch the whole discovery record to find one.
#[kynos::get(
    "/.well-known/capsule/deprecation",
    operation_id = "deprecation_announcements",
    tag = WellKnownTag
)]
pub async fn deprecation_announcements(
    Inject(discovery): Inject<DiscoveryContext>,
) -> Json<DeprecationsResponse> {
    Json(DeprecationsResponse {
        announcements: discovery
            .info()
            .deprecations()
            .iter()
            .map(announcement)
            .collect(),
    })
}

/// Serve the federation capability revocation list.
///
/// Bounded by at most 24 hours of revocations, because an entry past the token's own `exp` is
/// pruned and a capability token cannot be minted to live longer than that. Public: a peer
/// checking whether a token it holds is still good is, by construction, not yet authenticated
/// here, and the record names no user — only opaque `jti`s.
///
/// # Errors
///
/// Returns `503` if the revocation list cannot be read. Deliberately *not* an empty list: an
/// empty list is the strongest possible claim this endpoint can make — nothing is revoked — and
/// serving it on a storage failure would turn an outage into a silent un-revocation of every
/// token, which is exactly what the peer-side fail-closed rule exists to prevent.
#[kynos::get(
    "/.well-known/capsule/revoked-jti",
    operation_id = "revoked_jti",
    tag = WellKnownTag
)]
pub async fn revoked_jti(
    Inject(discovery): Inject<DiscoveryContext>,
) -> Result<Json<RevokedJtiResponse>, RevokedJtiError> {
    let published = discovery.revocations().published().await.map_err(|error| {
        tracing::error!(%error, "the revocation list could not be read");
        RevokedJtiError::Unavailable {
            code: error_codes::FEDERATION_REVOCATIONS_UNAVAILABLE,
        }
    })?;

    Ok(Json(RevokedJtiResponse {
        generated_at: published.generated_at.to_string(),
        max_staleness_seconds: u32::try_from(MAX_STALENESS.as_secs())
            .expect("fifteen minutes fits in a u32 of seconds"),
        revoked: published
            .revoked
            .into_iter()
            .map(|token| RevokedTokenResponse {
                jti: token.jti,
                expires_at: token.expires_at.to_string(),
            })
            .collect(),
    }))
}

/// The one way serving the revocation list can fail.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RevokedJtiError {
    /// The list could not be read, so no claim about it can be made.
    #[error("the revocation list could not be read")]
    #[problem(status = 503, title = "Revocation list unavailable")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// The wire projection of one announcement.
fn announcement(from: &DeprecationAnnouncement) -> DeprecationResponse {
    DeprecationResponse {
        min_protocol_version: from.min_protocol_version.clone(),
        announced_at: from.announced_at.to_string(),
        cutoff: from.cutoff.to_string(),
        detail_url: from.detail_url.clone(),
    }
}
