//! Provider metadata — the one document that tells the relying party where everything is.
//!
//! Fetched from `{issuer}/.well-known/openid-configuration` (OpenID Connect Discovery 1.0 §4),
//! **lazily and never at boot**: an identity provider that is down must not stop a server from
//! serving local auth. Cached for [`METADATA_TTL`] and refreshed on the next read after that.
//!
//! # Two refusals, both structural
//!
//! - **The document's `issuer` must equal the configured one.** Discovery 1.0 §4.3 requires it,
//!   and it is the mix-up defence: a document fetched from one origin that names another is a
//!   provider claiming to be somebody else, and honouring its endpoints would send the client
//!   secret and the authorization code wherever it said.
//! - **Every endpoint must be `https`**, unless the issuer itself is a loopback address — the
//!   development and test carve-out, stated here rather than left to a flag — and under that
//!   carve-out every plain-HTTP endpoint must **itself** be loopback: a loopback issuer whose
//!   document names an off-box `token_endpoint` would send the code and the verifier across the
//!   network in the clear. A token endpoint reached over plain HTTP is a client secret and an ID
//!   token on the wire in the clear. `localhost` is not loopback here, for the reason
//!   [`RedirectPolicy`](super::provider::RedirectPolicy) refuses it: a resolver can be made to
//!   send it elsewhere (RFC 8252 §8.3).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use crate::store::Clock;

/// How long fetched metadata is trusted before it is read again.
///
/// A day. Providers rotate endpoints rarely and announce it; what changes often — the signing
/// keys — has its own cache with its own trigger ([`super::jwks`]).
pub const METADATA_TTL: SignedDuration = SignedDuration::from_hours(24);

/// The provider facts this relying party reads. Everything else in the document is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProviderMetadata {
    /// The issuer the document claims to describe. Must equal the configured one.
    pub issuer: String,
    /// Where the person is sent to authenticate.
    pub authorization_endpoint: String,
    /// Where the authorization code is exchanged.
    pub token_endpoint: String,
    /// Where the signing keys are published.
    pub jwks_uri: String,
    /// Where a device authorization is requested (RFC 8628 §4). Absent means the provider does
    /// not offer the grant, which is a typed refusal at the device route, not a fault.
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

/// Why metadata could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// The document could not be fetched.
    #[error("the provider's discovery document could not be fetched: {detail}")]
    Unreachable {
        /// The transport's own description.
        detail: String,
    },
    /// The document is not the shape Discovery 1.0 describes.
    #[error("the provider's discovery document is not usable: {detail}")]
    Malformed {
        /// What was wrong with it.
        detail: String,
    },
    /// The document names an issuer other than the configured one.
    #[error("the discovery document names issuer {found:?}, not the configured issuer")]
    IssuerMismatch {
        /// The `issuer` the document carried.
        found: String,
    },
    /// An endpoint is not `https`, and the issuer is not a loopback address.
    #[error("the provider's {endpoint} is not https: {url}")]
    InsecureEndpoint {
        /// Which endpoint.
        endpoint: &'static str,
        /// The URL as published.
        url: String,
    },
}

/// The discovery URL for `issuer`, per Discovery 1.0 §4.1.
///
/// The issuer's own trailing slash is honoured rather than normalized — `issuer` is compared for
/// exact equality everywhere else, so this is the only place it is manipulated at all.
#[must_use]
pub fn discovery_url(issuer: &str) -> String {
    format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    )
}

/// Whether `issuer` is served from this machine — the one case plain HTTP is admitted.
///
/// The loopback IP literals only, never `localhost` (RFC 8252 §8.3): a name is resolved, and
/// a resolver can be made to answer with something that is not this machine.
#[must_use]
pub fn is_loopback_issuer(issuer: &str) -> bool {
    reqwest::Url::parse(issuer).is_ok_and(|url| {
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
    })
}

/// Check a fetched document against the configured `issuer`.
///
/// Pure, so the two refusals are unit tests.
///
/// # Errors
///
/// [`DiscoveryError::IssuerMismatch`] if the document is somebody else's;
/// [`DiscoveryError::InsecureEndpoint`] for an endpoint that is neither `https` nor — under a
/// loopback issuer — itself loopback `http`.
pub fn admit(metadata: ProviderMetadata, issuer: &str) -> Result<ProviderMetadata, DiscoveryError> {
    if metadata.issuer != issuer {
        return Err(DiscoveryError::IssuerMismatch {
            found: super::claims::bounded(&metadata.issuer),
        });
    }
    let endpoints = [
        (
            "authorization_endpoint",
            Some(&metadata.authorization_endpoint),
        ),
        ("token_endpoint", Some(&metadata.token_endpoint)),
        ("jwks_uri", Some(&metadata.jwks_uri)),
        (
            "device_authorization_endpoint",
            metadata.device_authorization_endpoint.as_ref(),
        ),
    ];
    for (endpoint, url) in endpoints {
        if let Some(url) = url {
            admit_url(endpoint, url, issuer)?;
        }
    }
    Ok(metadata)
}

/// Whether `url`, published by the provider for `issuer`, may be used: `https`, or — under a
/// loopback issuer — itself loopback `http`.
///
/// The one scheme rule, applied to every endpoint the discovery document names and to the
/// `verification_uri` a device authorization answers with, which is an address the person is
/// told to visit.
///
/// # Errors
///
/// [`DiscoveryError::InsecureEndpoint`] naming `endpoint`.
pub fn admit_url(endpoint: &'static str, url: &str, issuer: &str) -> Result<(), DiscoveryError> {
    let secure = url.starts_with("https://");
    let loopback = is_loopback_issuer(issuer) && is_loopback_issuer(url);
    if secure || loopback {
        return Ok(());
    }
    Err(DiscoveryError::InsecureEndpoint {
        endpoint,
        url: super::claims::bounded(url),
    })
}

/// One fetched document and when it was fetched.
#[derive(Debug)]
struct Cached {
    metadata: Arc<ProviderMetadata>,
    fetched_at: Timestamp,
}

/// The metadata cache for one configured issuer.
///
/// A `std` mutex rather than an async one, held only to read or replace the `Arc`: the fetch
/// happens outside it, so two concurrent misses fetch twice and the second write wins, which is
/// harmless for an idempotent document and cheaper than serializing every read behind a
/// network call.
#[derive(Debug)]
pub struct MetadataCache {
    issuer: String,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    cached: Mutex<Option<Cached>>,
}

impl MetadataCache {
    /// A cache for `issuer`, fetching with `http` and ageing by `clock`.
    pub fn new(issuer: impl Into<String>, http: reqwest::Client, clock: Arc<dyn Clock>) -> Self {
        Self {
            issuer: issuer.into(),
            http,
            clock,
            cached: Mutex::new(None),
        }
    }

    /// The configured issuer.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    fn slot(&self) -> MutexGuard<'_, Option<Cached>> {
        self.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current metadata, fetching it if the cache is empty or older than [`METADATA_TTL`].
    ///
    /// # Errors
    ///
    /// The fetch's [`DiscoveryError`], if one was needed and failed. A stale document is **not**
    /// served on a failed refresh: endpoints are where secrets are sent, and a day-old answer
    /// to "where is the token endpoint" is a day-old fact about where to send them.
    pub async fn current(&self) -> Result<Arc<ProviderMetadata>, DiscoveryError> {
        let now = self.clock.now();
        if let Some(cached) = self.slot().as_ref()
            && now.duration_since(cached.fetched_at) < METADATA_TTL
        {
            return Ok(Arc::clone(&cached.metadata));
        }

        tracing::info!(issuer = %self.issuer, "fetching the provider's discovery document");
        let fetched = self.fetch().await?;
        let metadata = Arc::new(admit(fetched, &self.issuer)?);
        *self.slot() = Some(Cached {
            metadata: Arc::clone(&metadata),
            fetched_at: now,
        });
        Ok(metadata)
    }

    async fn fetch(&self) -> Result<ProviderMetadata, DiscoveryError> {
        let response = self
            .http
            .get(discovery_url(&self.issuer))
            .send()
            .await
            .map_err(|error| DiscoveryError::Unreachable {
                detail: error.to_string(),
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(DiscoveryError::Unreachable {
                detail: format!("the discovery endpoint answered {status}"),
            });
        }
        response
            .json::<ProviderMetadata>()
            .await
            .map_err(|error| DiscoveryError::Malformed {
                detail: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests;
