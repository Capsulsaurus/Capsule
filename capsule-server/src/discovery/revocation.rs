//! The federation capability revocation list — publishing it, and reading someone else's.
//!
//! # Two halves of one rule
//!
//! design/federation.md gives revocation a shape that only works if both ends implement it:
//! the issuer publishes revoked `jti`s at `/.well-known/capsule/revoked-jti`, and a verifier
//! caches that list with a **maximum staleness of 15 minutes**. A token revoked one minute ago
//! is still honored by a peer that fetched the list two minutes ago — that latency is the
//! deliberate price of not making every verification a network call.
//!
//! The half that is easy to leave out is the failure case, and it is the one that matters:
//! **list unavailability fails closed.** A verifier that cannot refresh a cached list must, past
//! the 15-minute bound, reject tokens it can no longer confirm. Without that rule revocation is
//! defeated by making the list unreachable, which is a capability any network position between
//! the two servers already has. So both halves live here — [`RevocationList`] publishes,
//! [`check_revocation`] reads — and the fail-closed case is a variant of the verdict rather
//! than an error path a caller might treat as "carry on".
//!
//! # Why the list stays bounded without anything sweeping it
//!
//! A capability token's `exp` is capped at 24 hours, and an expired token is rejected
//! unconditionally whether or not it appears here — so an entry whose `exp` has passed carries
//! no information and is pruned on read. That makes the published list bounded by at most 24
//! hours of revocations as a *consequence* of the TTL ceiling rather than as a size limit
//! somebody has to enforce. [`RevocationList::revoke`] refuses an entry whose expiry is beyond
//! the ceiling, which is what keeps that reasoning true: one accepted long-lived entry and the
//! list grows without bound while the peer-side staleness math silently stops applying.
//!
//! # Where the list lives now
//!
//! The port is implemented by the federation capability store
//! ([`crate::federation::CapabilityStore`]), because once this server *issues* capabilities the
//! record of one and the fact of its revocation are one row, and a standalone list would be a
//! second answer to "is this `jti` revoked". The deterministic adapter is
//! [`crate::federation::InMemoryCapabilities`]; the conformance suite that pins the pruning,
//! ordering and ceiling rules is `federation::conformance`.

use std::fmt;
use std::pin::Pin;

use jiff::{SignedDuration, Timestamp};

use crate::store::{StoreError, StoreFuture};

/// The ceiling design/federation.md puts on a capability token's lifetime.
pub const MAX_TOKEN_TTL: SignedDuration = SignedDuration::from_hours(24);

/// How stale a cached copy of a peer's list may be before it stops being usable.
pub const MAX_STALENESS: SignedDuration = SignedDuration::from_mins(15);

/// One revoked capability token.
///
/// The `jti` and nothing else about the token: the list is consulted by peers, and a record
/// naming the album or the user a revoked token covered would leak exactly what the registry's
/// no-enumeration rule forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedToken {
    /// The token's `jti` claim.
    pub jti: String,
    /// The token's own `exp`. After this the entry is redundant and is pruned.
    pub expires_at: Timestamp,
}

/// A published revocation list, as of the moment it was generated.
///
/// `generated_at` is part of the record rather than inferred from an HTTP `Date`, because the
/// staleness rule is a property of the list's content and a verifier that reasoned from a
/// transport header would be trusting a cache to be honest about age.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedRevocations {
    /// When this snapshot was taken.
    pub generated_at: Timestamp,
    /// Every revoked token not yet past its own expiry, oldest expiry first.
    pub revoked: Vec<RevokedToken>,
}

/// Why an entry was refused on its own terms.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RevocationError {
    /// The entry's expiry is further out than a capability token is allowed to live.
    ///
    /// The list's bound is a consequence of this ceiling, so accepting one entry past it does
    /// not merely admit a bad record — it quietly invalidates the reasoning that keeps the
    /// published list small enough for a peer to fetch every fifteen minutes.
    #[error("a capability token cannot expire at {expires_at}, beyond the {ceiling} ceiling")]
    BeyondTtlCeiling {
        /// The refused expiry.
        expires_at: Timestamp,
        /// The ceiling it exceeded.
        ceiling: SignedDuration,
    },
}

/// What can go wrong recording a revocation.
///
/// Two kinds, kept apart: the entry was inadmissible, or the store could not be reached. An
/// operator retrying the first will retry forever; the second is exactly what a retry is for.
#[derive(Debug, thiserror::Error)]
pub enum RevokeError {
    /// The entry itself was refused.
    #[error(transparent)]
    Refused(#[from] RevocationError),
    /// The list could not be written.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The future [`RevocationList::revoke`] returns.
pub type RevokeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), RevokeError>> + Send + 'a>>;

/// Publishing side: the revocations this server has issued.
///
/// A port rather than a value because it is written on every revocation and read on every peer
/// fetch — the same reason the auth state store is one.
pub trait RevocationList: fmt::Debug + Send + Sync {
    /// Record that `jti` is revoked until its own `expires_at`.
    ///
    /// Idempotent: revoking the same `jti` twice is one entry, because a revocation is a fact
    /// about a token rather than an event, and a retried administrative action must not be
    /// distinguishable from a single one.
    ///
    /// # Errors
    ///
    /// Returns [`RevokeError::Refused`] if the entry's expiry is beyond the capability TTL
    /// ceiling, and [`RevokeError::Store`] if the list cannot be written.
    fn revoke(&self, token: RevokedToken) -> RevokeFuture<'_>;

    /// The list as it stands, pruned of entries whose expiry has passed.
    fn published(&self) -> StoreFuture<'_, PublishedRevocations>;
}

/// What a verifier concluded about one `jti`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationVerdict {
    /// The token is not revoked and the list backing that answer is fresh enough to rely on.
    Honored,
    /// The token appears on the list.
    Revoked,
    /// The token has outlived its own `exp`, which is refused whatever the list says.
    Expired,
    /// The list is older than the staleness bound and could not be refreshed.
    ///
    /// A refusal, not a degraded acceptance. This is the variant that keeps revocation from
    /// being defeated by making the list unreachable.
    Stale,
}

impl RevocationVerdict {
    /// Whether the token may be used.
    pub fn accepts(self) -> bool {
        matches!(self, Self::Honored)
    }
}

/// Verifying side: decide whether to honor `jti` against a peer's published list.
///
/// `expires_at` is the token's own `exp`, checked first because an expired token is refused
/// unconditionally and no list can rehabilitate it. Membership comes next and staleness last,
/// which is not the order the rule is written in but is the order that reports the most
/// specific true reason: a listed `jti` is revoked whether or not the copy naming it is stale,
/// and both answers are refusals. What staleness actually guards is the *absence* of an entry —
/// the one answer an out-of-date list must not be believed about.
pub fn check_revocation(
    list: &PublishedRevocations,
    jti: &str,
    expires_at: Timestamp,
    now: Timestamp,
) -> RevocationVerdict {
    if expires_at <= now {
        return RevocationVerdict::Expired;
    }

    if list.revoked.iter().any(|token| token.jti == jti) {
        return RevocationVerdict::Revoked;
    }

    if now.duration_since(list.generated_at) > MAX_STALENESS {
        tracing::warn!(
            jti = %jti,
            generated_at = %list.generated_at,
            "a capability token is refused: the issuer's revocation list is past the staleness bound"
        );
        return RevocationVerdict::Stale;
    }

    RevocationVerdict::Honored
}
