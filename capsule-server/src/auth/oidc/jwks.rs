//! The provider's signing keys, cached and refreshed on evidence.
//!
//! # Refreshed on an unknown `kid`, and floored
//!
//! A provider that rotates its keys announces it by signing with a `kid` the relying party has
//! not seen, so an unknown `kid` is the trigger for a refetch. It is also what a forger sends,
//! so the refetch is floored at one per [`REFRESH_FLOOR`]: a stream of tokens with invented
//! key ids cannot make this server hammer the provider's JWKS endpoint, and a burst of
//! suppressed refetches in the log is a probe worth reading about.
//!
//! # Stale rather than empty
//!
//! A failed refresh keeps the previous set. A key set that was good a minute ago is still the
//! provider's — the failure mode to avoid is the one where a transient network fault turns every
//! sign-in into a `500`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::jwk::JwkSet;

use crate::store::Clock;

/// The shortest interval between two refetches of the key set.
pub const REFRESH_FLOOR: SignedDuration = SignedDuration::from_secs(60);

/// Why the key set could not be fetched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The JWKS endpoint could not be reached, or answered with an error.
    #[error("the provider's key set could not be fetched: {detail}")]
    Unreachable {
        /// The transport's own description.
        detail: String,
    },
    /// The document is not a JWK Set.
    #[error("the provider's key set is not usable: {detail}")]
    Malformed {
        /// What was wrong with it.
        detail: String,
    },
}

/// What a refetch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// The set was fetched again, and this is it.
    Fetched(Arc<JwkSet>),
    /// A fetch happened inside the floor; the cached set stands.
    Suppressed,
}

#[derive(Debug)]
struct Cached {
    keys: Arc<JwkSet>,
    fetched_at: Timestamp,
}

/// The key cache for one provider.
#[derive(Debug)]
pub struct KeyCache {
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    cached: Mutex<Option<Cached>>,
}

impl KeyCache {
    /// An empty cache fetching with `http` and ageing by `clock`.
    pub fn new(http: reqwest::Client, clock: Arc<dyn Clock>) -> Self {
        Self {
            http,
            clock,
            cached: Mutex::new(None),
        }
    }

    fn slot(&self) -> MutexGuard<'_, Option<Cached>> {
        self.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current key set, fetched from `jwks_uri` if none is cached.
    ///
    /// No age-based expiry: a key set is refreshed on evidence ([`Self::refresh`]), not on a
    /// timer, because the only observable fact about a rotation is a token the cached set
    /// cannot verify.
    ///
    /// # Errors
    ///
    /// The fetch's [`KeyError`] if the cache was empty and the fetch failed.
    pub async fn current(&self, jwks_uri: &str) -> Result<Arc<JwkSet>, KeyError> {
        if let Some(cached) = self.slot().as_ref() {
            return Ok(Arc::clone(&cached.keys));
        }
        tracing::info!("fetching the provider's key set for the first time");
        self.fetch_and_store(jwks_uri).await
    }

    /// Refetch the key set because a token named a key the cached set does not hold.
    ///
    /// Returns [`Refresh::Suppressed`] — and logs a `WARN` — when the last fetch was less than
    /// [`REFRESH_FLOOR`] ago. A failed fetch keeps the cached set and returns the error.
    ///
    /// # Errors
    ///
    /// The fetch's [`KeyError`].
    pub async fn refresh(&self, jwks_uri: &str) -> Result<Refresh, KeyError> {
        let now = self.clock.now();
        if let Some(cached) = self.slot().as_ref()
            && now.duration_since(cached.fetched_at) < REFRESH_FLOOR
        {
            tracing::warn!(
                "an ID token named an unknown key inside the refetch floor; a burst of these is \
                 a forged-kid probe"
            );
            return Ok(Refresh::Suppressed);
        }
        tracing::info!("refetching the provider's key set after an unknown kid");
        self.fetch_and_store(jwks_uri).await.map(Refresh::Fetched)
    }

    async fn fetch_and_store(&self, jwks_uri: &str) -> Result<Arc<JwkSet>, KeyError> {
        let now = self.clock.now();
        let response =
            self.http
                .get(jwks_uri)
                .send()
                .await
                .map_err(|error| KeyError::Unreachable {
                    detail: error.to_string(),
                })?;
        let status = response.status();
        if !status.is_success() {
            return Err(KeyError::Unreachable {
                detail: format!("the JWKS endpoint answered {status}"),
            });
        }
        let keys = response
            .json::<JwkSet>()
            .await
            .map_err(|error| KeyError::Malformed {
                detail: error.to_string(),
            })?;
        let keys = Arc::new(keys);
        *self.slot() = Some(Cached {
            keys: Arc::clone(&keys),
            fetched_at: now,
        });
        tracing::debug!(keys = keys.keys.len(), "cached the provider's key set");
        Ok(keys)
    }
}
