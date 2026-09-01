//! The verifier-side revocation-list view with **fail-closed staleness** (slice `S-E2`;
//! SSoT: [Federation — Token Lifecycle](https://docs/design/federation/#token-lifecycle-and-chain-of-trust)).
//!
//! Revocation is a short TTL (`exp ≤ 24h`) plus a published `/.well-known/capsule/revoked-jti`
//! list. A verifier caches an issuer's list with a **maximum staleness of 15 minutes**. The
//! critical rule this type enforces: **list unavailability fails closed.** A verifier relying on
//! a cached copy it cannot refresh must, past the 15-minute bound, reject any token whose `jti`
//! it can no longer confirm — it never honors tokens indefinitely on a stale list. A server
//! verifying its *own* tokens ([`RevocationList::owned`]) checks its own always-fresh list and is
//! never stale.

use std::collections::HashSet;

use jiff::{SignedDuration, Timestamp};

/// The default maximum staleness a cached remote revocation list is trusted for.
#[must_use]
pub fn default_max_staleness() -> SignedDuration {
    SignedDuration::from_mins(15)
}

/// The outcome of confirming a `jti` against a revocation list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationVerdict {
    /// The `jti` is not on the (confirmable) list — the token may proceed.
    NotRevoked,
    /// The `jti` is on the list — revoked.
    Revoked,
    /// The cached list is too stale to trust and could not be refreshed — fail closed, the
    /// token cannot be confirmed and must be rejected.
    Unverifiable,
}

/// Freshness provenance of a revocation-list view.
#[derive(Debug, Clone)]
enum Freshness {
    /// The issuer's own list, authoritative and always fresh.
    Own,
    /// A cached copy of a remote issuer's list, last successfully fetched at this instant.
    Cached {
        fetched_at: Timestamp,
        max_staleness: SignedDuration,
    },
}

/// A revocation-list view: the set of active revoked `jti`s plus its freshness.
#[derive(Debug, Clone)]
pub struct RevocationList {
    revoked: HashSet<String>,
    freshness: Freshness,
}

impl RevocationList {
    /// The issuer's own always-fresh list, built from its durably-stored active `jti`s. Never
    /// goes stale — an issuer verifying its own tokens always sees a current list.
    pub fn owned<I, S>(active_jtis: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            revoked: active_jtis.into_iter().map(Into::into).collect(),
            freshness: Freshness::Own,
        }
    }

    /// A cached copy of a *remote* issuer's list, last fetched at `fetched_at`, trusted for at
    /// most `max_staleness`. Past that bound, with no successful refresh, every `jti` becomes
    /// [`RevocationVerdict::Unverifiable`] — the fail-closed rule.
    pub fn cached<I, S>(
        active_jtis: I,
        fetched_at: Timestamp,
        max_staleness: SignedDuration,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            revoked: active_jtis.into_iter().map(Into::into).collect(),
            freshness: Freshness::Cached {
                fetched_at,
                max_staleness,
            },
        }
    }

    /// Record a successful refresh at `fetched_at` with a fresh set of active `jti`s. Resets the
    /// staleness clock. No-op for an owned (already always-fresh) list.
    pub fn refresh<I, S>(&mut self, active_jtis: I, fetched_at: Timestamp)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.revoked = active_jtis.into_iter().map(Into::into).collect();
        if let Freshness::Cached { fetched_at: at, .. } = &mut self.freshness {
            *at = fetched_at;
        }
    }

    /// Confirm a `jti` against the list as of `now`. A cached list past its staleness bound
    /// returns [`RevocationVerdict::Unverifiable`] for **every** token — it can no longer confirm
    /// any `jti`, so it fails closed.
    #[must_use]
    pub fn check(&self, jti: &str, now: Timestamp) -> RevocationVerdict {
        if let Freshness::Cached {
            fetched_at,
            max_staleness,
        } = &self.freshness
            && now.as_second() - fetched_at.as_second() > max_staleness.as_secs()
        {
            tracing::warn!(
                jti,
                "revocation list is stale and could not be refreshed; failing closed"
            );
            return RevocationVerdict::Unverifiable;
        }
        if self.revoked.contains(jti) {
            RevocationVerdict::Revoked
        } else {
            RevocationVerdict::NotRevoked
        }
    }

    /// The number of active revoked `jti`s the view holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.revoked.len()
    }

    /// Whether the view holds no revocations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.revoked.is_empty()
    }
}
