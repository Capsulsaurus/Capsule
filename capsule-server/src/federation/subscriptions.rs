//! Local album subscriptions: "this server pulls album A from peer P for local user U, under
//! capability C" (design/federation.md, Token Lifecycle step 1; #1035).
//!
//! # Where a subscription comes from
//!
//! An album's owner mints a capability on the album's home server for **this** server and the
//! member it carries (`POST /v1/albums/{album_id}/capabilities` there). The token travels in the
//! share invite to the member's client, which posts it here, to the member's own home server
//! (`POST /v1/federation/subscriptions`). This module is what that post becomes: a durable row
//! the egress pull worker reads its jobs from.
//!
//! The token is **verified before it is stored**, against the issuer's operator-pinned key
//! ([`verify_received`](super::capability::verify_received)): its `iss` must be the peer the
//! client named and its `sub` must be this server. A capability this server could not present
//! is never recorded, so nothing a client invents becomes a job.
//!
//! # One row per subscriber, peer and album
//!
//! Keyed `(subscriber, peer, album)`. Posting a grant for a key already held keeps whichever
//! token lives longer ([`SubscriptionStore::put`]), so a client replaying an old invite never
//! replaces the successor a refresh stored, and a fresh invite revives a subscription whose
//! token lapsed.
//!
//! # Jobs are albums, not subscribers
//!
//! The pull worker's unit is a [`JobKey`]: one album and its home server. Two local members
//! subscribed to one album are one job, because the album's assets are the same bytes whoever
//! asked for them; [`SubscriptionStore::live_jobs`] answers the distinct keys, and
//! [`SubscriptionStore::of_job`] the subscriptions a pull may present.
//!
//! # A lapsed token is kept, not pulled
//!
//! A token is honoured for at most 24 hours, and only a refresh by the puller, or a fresh invite,
//! extends it. A subscription whose token has lapsed is still listed to its subscriber, so a client
//! can tell that it needs a new invite, but it is no job: its issuer would refuse every request
//! presenting it.
//!
//! # What lives where
//!
//! - This file — the record, the port and [`SubscriptionJobs`], the production
//!   [`JobSource`].
//! - [`memory`] — [`InMemorySubscriptions`], the deterministic double.
//! - [`postgres`] — [`PostgresSubscriptions`](postgres::PostgresSubscriptions), the durable
//!   adapter over `federation_subscriptions`.
//! - [`conformance`] — the suite both adapters pass.

use std::fmt;
use std::sync::Arc;

use jiff::Timestamp;

use super::PeerId;
use super::capability::Scope;
use super::pull::{JobKey, JobSource};
use crate::store::{AlbumId, Clock, StoreFuture, UserId};

pub mod conformance;
pub mod memory;
pub mod postgres;

pub use self::memory::InMemorySubscriptions;

/// The longest capability token intake reads, in bytes.
///
/// A token is nine short claims under an Ed25519 signature, well under a kibibyte; four is room
/// for any issuer's spelling of them and no room for a megabyte stored per row.
pub const MAX_CAPABILITY_BYTES: usize = 4096;

/// A capability token as a subscription holds it.
///
/// Its own type so nothing prints it: the token is a bearer credential on its issuer, and a
/// derived `Debug` would publish it to any `tracing` field that formatted a subscription. It is
/// useless without this server's request-signing key, which is why it may be kept at all.
#[derive(Clone, PartialEq, Eq)]
pub struct CapabilityToken(String);

impl CapabilityToken {
    /// Wraps a token.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token, to present as `Authorization: Bearer`.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CapabilityToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CapabilityToken(<redacted>)")
    }
}

/// One local album subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// The local account the album is pulled for.
    pub subscriber: UserId,
    /// The album's home server: the capability's issuer, and what every pull fetches from.
    pub peer: PeerId,
    /// The album, as its home server names it.
    pub album: AlbumId,
    /// What the capability permits.
    pub scope: Scope,
    /// The capability's `jti`.
    pub jti: String,
    /// The capability itself.
    pub token: CapabilityToken,
    /// When the capability was issued.
    pub issued_at: Timestamp,
    /// When the capability stops being honoured.
    pub expires_at: Timestamp,
    /// The album's pinned protocol date, which a pull must speak.
    pub min_protocol_version: String,
    /// When this subscription was first recorded.
    pub subscribed_at: Timestamp,
    /// When its capability was last replaced.
    pub updated_at: Timestamp,
}

impl Subscription {
    /// The job this subscription feeds.
    pub fn job(&self) -> JobKey {
        JobKey::new(self.peer.clone(), self.album.clone())
    }

    /// Whether its capability is still honoured at `now`.
    pub fn is_live(&self, now: Timestamp) -> bool {
        now < self.expires_at
    }
}

/// What [`SubscriptionStore::put`] did, with the subscription as it now stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutOutcome {
    /// No subscription held the key; this one is recorded.
    Created(Subscription),
    /// One did, and the posted capability outlives its token, so the token was replaced. The
    /// stored `subscribed_at` is kept.
    Replaced(Subscription),
    /// One did, and its token lives at least as long as the posted one. Nothing changed.
    Kept(Subscription),
}

impl PutOutcome {
    /// The subscription as it now stands.
    pub fn subscription(&self) -> &Subscription {
        match self {
            Self::Created(subscription)
            | Self::Replaced(subscription)
            | Self::Kept(subscription) => subscription,
        }
    }

    /// The name this outcome travels under, for a log field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created(_) => "created",
            Self::Replaced(_) => "replaced",
            Self::Kept(_) => "kept",
        }
    }
}

/// Where local album subscriptions are kept.
pub trait SubscriptionStore: fmt::Debug + Send + Sync {
    /// Record `subscription` under its `(subscriber, peer, album)` key.
    ///
    /// Atomic: with a subscription already under the key, its token is replaced only when the
    /// posted one's `expires_at` is later, and its `subscribed_at` is kept either way.
    fn put(&self, subscription: Subscription) -> StoreFuture<'_, PutOutcome>;

    /// Every subscription of `subscriber`, live or lapsed, ordered by peer and then album.
    fn of_subscriber<'a>(&'a self, subscriber: &'a UserId) -> StoreFuture<'a, Vec<Subscription>>;

    /// Forget `subscriber`'s subscription to `album` on `peer`. Whether there was one.
    fn remove<'a>(
        &'a self,
        subscriber: &'a UserId,
        peer: &'a PeerId,
        album: &'a AlbumId,
    ) -> StoreFuture<'a, bool>;

    /// Every subscription feeding `job`, live or lapsed, ordered by subscriber.
    fn of_job<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, Vec<Subscription>>;

    /// Every job with at least one subscription live at `now`, each once, in [`JobKey`] order.
    fn live_jobs(&self, now: Timestamp) -> StoreFuture<'_, Vec<JobKey>>;
}

/// The production [`JobSource`]: the jobs every live subscription feeds, at the clock's now.
#[derive(Debug, Clone)]
pub struct SubscriptionJobs {
    store: Arc<dyn SubscriptionStore>,
    clock: Arc<dyn Clock>,
}

impl SubscriptionJobs {
    /// The jobs `store` holds, read at `clock`'s now.
    pub fn new(store: Arc<dyn SubscriptionStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }
}

impl JobSource for SubscriptionJobs {
    fn jobs(&self) -> StoreFuture<'_, Vec<JobKey>> {
        Box::pin(async move {
            let jobs = self.store.live_jobs(self.clock.now()).await?;
            tracing::debug!(jobs = jobs.len(), "listed the live subscriptions' jobs");
            Ok(jobs)
        })
    }
}

#[cfg(test)]
mod tests;
