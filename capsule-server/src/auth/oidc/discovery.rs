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
//!   development and test carve-out, stated here rather than left to a flag. A token endpoint
//!   reached over plain HTTP is a client secret and an ID token on the wire in the clear.

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
#[must_use]
pub fn is_loopback_issuer(issuer: &str) -> bool {
    reqwest::Url::parse(issuer).is_ok_and(|url| {
        url.scheme() == "http"
            && matches!(
                url.host_str(),
                Some("127.0.0.1" | "[::1]" | "::1" | "localhost")
            )
    })
}

/// Check a fetched document against the configured `issuer`.
///
/// Pure, so the two refusals are unit tests.
///
/// # Errors
///
/// [`DiscoveryError::IssuerMismatch`] if the document is somebody else's;
/// [`DiscoveryError::InsecureEndpoint`] for a plain-HTTP endpoint on a non-loopback issuer.
pub fn admit(metadata: ProviderMetadata, issuer: &str) -> Result<ProviderMetadata, DiscoveryError> {
    if metadata.issuer != issuer {
        return Err(DiscoveryError::IssuerMismatch {
            found: metadata.issuer,
        });
    }
    if !is_loopback_issuer(issuer) {
        for (endpoint, url) in [
            ("authorization_endpoint", &metadata.authorization_endpoint),
            ("token_endpoint", &metadata.token_endpoint),
            ("jwks_uri", &metadata.jwks_uri),
        ] {
            if !url.starts_with("https://") {
                return Err(DiscoveryError::InsecureEndpoint {
                    endpoint,
                    url: url.clone(),
                });
            }
        }
    }
    Ok(metadata)
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
mod tests {
    use super::*;

    fn metadata(issuer: &str, scheme: &str) -> ProviderMetadata {
        ProviderMetadata {
            issuer: issuer.to_owned(),
            authorization_endpoint: format!("{scheme}://idp.example.test/auth"),
            token_endpoint: format!("{scheme}://idp.example.test/token"),
            jwks_uri: format!("{scheme}://idp.example.test/keys"),
        }
    }

    #[test]
    fn the_discovery_url_hangs_off_the_issuer() {
        assert_eq!(
            discovery_url("https://idp.example.test"),
            "https://idp.example.test/.well-known/openid-configuration"
        );
        assert_eq!(
            discovery_url("https://idp.example.test/realm/"),
            "https://idp.example.test/realm/.well-known/openid-configuration"
        );
    }

    #[test]
    fn a_document_naming_another_issuer_is_refused() {
        let error = admit(
            metadata("https://somebody-else.test", "https"),
            "https://idp.example.test",
        )
        .expect_err("refused");
        assert_eq!(
            error,
            DiscoveryError::IssuerMismatch {
                found: "https://somebody-else.test".to_owned()
            }
        );
    }

    #[test]
    fn plain_http_endpoints_are_refused_unless_the_issuer_is_loopback() {
        let error = admit(
            metadata("https://idp.example.test", "http"),
            "https://idp.example.test",
        )
        .expect_err("refused");
        assert!(matches!(
            error,
            DiscoveryError::InsecureEndpoint {
                endpoint: "authorization_endpoint",
                ..
            }
        ));

        for issuer in [
            "http://127.0.0.1:5556/dex",
            "http://[::1]:5556",
            "http://localhost:5556",
        ] {
            assert!(is_loopback_issuer(issuer), "{issuer}");
            assert!(admit(metadata(issuer, "http"), issuer).is_ok(), "{issuer}");
        }
        assert!(
            !is_loopback_issuer("https://127.0.0.1"),
            "https is not the carve-out"
        );
        assert!(!is_loopback_issuer("http://idp.example.test"));
    }
}
