//! Server-to-server federation (`S-E2`, `S-E5`, `S-C49`): the capability that gates which peer
//! may pull which album, the store it is issued from and revoked into, and the peers this
//! server knows.
//!
//! # No new data protocol
//!
//! design/federation.md is explicit: a peer fetches *exactly* the primitives a client fetches —
//! `GET /v1/sync?album_id=…` and `GET /v1/blob/{hash}` — and what federation adds is the
//! **capability token** those two reads accept in the `Authorization: Bearer` slot, plus the
//! per-peer budget behind it. So there is no `/v1/federation/pull` here and never will be: the
//! pull path is the read path, and this module is the credential, the lifecycle around it
//! (mint, refresh, revoke) and the moderation halves that hang on it (signed report intake, the
//! server-level blocklist).
//!
//! # What lives where
//!
//! - [`capability`] — the EdDSA-JWT and the codec that mints and reads it, over the **same**
//!   Ed25519 key the session tokens are signed with, which is the key `server-info` publishes.
//! - [`store`] — [`CapabilityStore`], the record of every capability this server issued. It
//!   **is** the revocation list: the adapters implement
//!   [`RevocationList`](crate::discovery::revocation::RevocationList) and
//!   `/.well-known/capsule/revoked-jti` reads them, so "is this `jti` revoked" has one answer.
//! - [`peers`] — [`PeerStore`], the peers whose signing keys an operator has pinned and the
//!   blocklist, which is a column on the same row.
//! - [`memory`] — the deterministic doubles; [`conformance`] — the suite every adapter passes.
//!
//! # A peer is not an account
//!
//! A [`PeerId`] is a server's canonical origin (`other.tld`), never a user id, and the types
//! keep them apart everywhere the two could be confused: the sync cursor's scope byte, the
//! blob authority's principal, the counter key. Nothing here holds a user list, and nothing
//! published here names a user — the registry's no-enumeration rule holds at this layer too.

use std::fmt;
use std::sync::Arc;

pub mod capability;
pub mod conformance;
pub mod memory;
pub mod peers;
pub mod scheme;
pub mod store;

pub use self::capability::{
    ALBUM_URN_PREFIX, CapabilityCodec, CapabilityError, CapabilityGrant, MintError, MintRequest,
    Minted, Scope, album_from_urn, album_urn,
};
pub use self::memory::{InMemoryCapabilities, InMemoryPeers};
pub use self::peers::{BlockOutcome, PeerRecord, PeerStore, UnblockOutcome};
pub use self::scheme::{Principal, ReadBearer, VerifiedCapability};
pub use self::store::{
    CapabilityFilter, CapabilityRecord, CapabilityStore, RefreshOutcome, RevokeOutcome,
};
use crate::counter::{CounterContext, CounterKey, budgets};
use crate::store::Clock;

/// A peer server's identity: its canonical origin, as its own `server-info` publishes it.
///
/// Its own type rather than a `UserId` or a bare string so a peer can never be handed to a port
/// that expects an account, and so the log field that names one reads as what it is.
///
/// Canonical: a host name is case-insensitive and a trailing dot names the same host, so both
/// are folded at construction. A block on `other.test` therefore covers a capability minted for
/// `Other.Test.`, and two records can never name one peer twice.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(String);

impl PeerId {
    /// Wraps an origin, folded to its canonical form.
    pub fn new(value: impl Into<String>) -> Self {
        let value: String = value.into();
        Self(value.trim().trim_end_matches('.').to_ascii_lowercase())
    }

    /// The origin as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({:?})", self.0)
    }
}

/// What the federation module is assembled from.
///
/// Named rather than positional, for the reason [`crate::app::Modules`] is: a constructor that
/// lengthens with every collaborator is one that is eventually got wrong positionally.
#[derive(Debug)]
pub struct FederationCollaborators {
    /// Mints and reads capability tokens.
    pub codec: Arc<CapabilityCodec>,
    /// Every capability this server issued, and the revocation list it publishes.
    pub capabilities: Arc<dyn CapabilityStore>,
    /// The peers this server has pinned or blocked.
    pub peers: Arc<dyn PeerStore>,
    /// The clock every record and every deadline is stamped from.
    pub clock: Arc<dyn Clock>,
    /// Where peers reach this server, when it federates at all.
    ///
    /// `None` is a deployment that does not federate: the lifecycle writes refuse with
    /// `error.federation.not_configured`, while a capability minted earlier still verifies —
    /// a token is not un-minted by a configuration change.
    pub federation_url: Option<String>,
}

/// The federation module's collaborators.
#[derive(Debug, Clone)]
pub struct FederationContext {
    codec: Arc<CapabilityCodec>,
    capabilities: Arc<dyn CapabilityStore>,
    peers: Arc<dyn PeerStore>,
    clock: Arc<dyn Clock>,
    federation_url: Option<String>,
}

impl FederationContext {
    /// Assembles the module.
    pub fn new(collaborators: FederationCollaborators) -> Self {
        let FederationCollaborators {
            codec,
            capabilities,
            peers,
            clock,
            federation_url,
        } = collaborators;
        Self {
            codec,
            capabilities,
            peers,
            clock,
            federation_url,
        }
    }

    /// The codec capabilities are minted with and read by.
    pub fn codec(&self) -> &CapabilityCodec {
        &self.codec
    }

    /// Every capability this server issued.
    pub fn capabilities(&self) -> &dyn CapabilityStore {
        self.capabilities.as_ref()
    }

    /// The peers this server knows.
    pub fn peers(&self) -> &dyn PeerStore {
        self.peers.as_ref()
    }

    /// The clock.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Where peers reach this server, if it federates.
    pub fn federation_url(&self) -> Option<&str> {
        self.federation_url.as_deref()
    }

    /// Whether this deployment federates at all.
    ///
    /// The gate on every lifecycle write. Reads are not gated on it: a capability that was
    /// minted while federation was on still verifies, and refusing it would cut a peer off
    /// without a revocation anybody can see.
    pub fn is_configured(&self) -> bool {
        self.federation_url.is_some()
    }
}

/// Why an admitted capability is refused by a route.
///
/// Every variant is a *coded* answer the route renders — the authenticator has no seam for one
/// (see [`scheme`]). The order [`admit`] decides them in is the order a client should learn them:
/// a revoked grant is refused before anything is charged to the peer's budget, and a blocked
/// peer is refused before it is either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The capability's `jti` is revoked. `403 error.federation.capability_revoked`.
    Revoked,
    /// The peer is on this server's blocklist. `403 error.moderation.server_blocked`.
    PeerBlocked,
    /// The peer's events-per-hour budget is spent. `429 error.federation.rate_budget_exceeded`.
    RateLimited {
        /// When the window resets.
        retry_after: jiff::Timestamp,
    },
    /// A collaborator could not answer, so nothing was decided. `500 error.federation.unavailable`.
    ///
    /// Never an admission: a limiter that fails open is a limiter an attacker turns off by
    /// loading the counter store.
    Unavailable,
}

/// What a capability is being presented for.
///
/// Only the liveness rule differs, and it differs for one reason: a predecessor that was revoked
/// **because it was refreshed** is exactly what a replayed refresh looks like, and refusing it
/// would make the idempotency the contract promises unreachable. Every other revocation refuses
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    /// A read: a sync page or a blob fetch. The grant must be live.
    Read,
    /// A refresh, presenting the predecessor. A predecessor already linked to a successor is
    /// admitted so the replay can be answered with that successor — whose own liveness the
    /// route then asks about, because a block cascades over it too.
    Refresh,
}

/// Decide whether `capability` may be presented at all, and charge the peer's budget if so.
///
/// The three questions every federated request asks before it looks at what is being asked for:
/// is the grant still live, is the peer still welcome, and is the peer within budget. Asked
/// here once so the sync, blob and refresh routes cannot ask them in different orders.
///
/// # Errors
///
/// Returns the [`Refusal`] the route renders.
pub async fn admit(
    federation: &FederationContext,
    counters: &CounterContext,
    capability: &VerifiedCapability,
    presentation: Presentation,
) -> Result<(), Refusal> {
    let peer = &capability.record.peer_id;
    let live = match presentation {
        Presentation::Read => capability.record.is_live(federation.clock().now()),
        Presentation::Refresh => {
            capability.record.refreshed_to.is_some()
                || capability.record.is_live(federation.clock().now())
        }
    };
    if !live {
        tracing::info!(
            %peer,
            jti = %capability.record.jti,
            "a revoked capability was presented"
        );
        return Err(Refusal::Revoked);
    }

    let blocked = federation
        .peers()
        .read(peer)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the peer store could not answer an admission");
            Refusal::Unavailable
        })?
        .is_some_and(|record| record.is_blocked());
    if blocked {
        tracing::info!(%peer, "a blocked peer presented a capability");
        return Err(Refusal::PeerBlocked);
    }

    let key = CounterKey::PeerRequests(peer.as_str().to_owned());
    match counters
        .hit(&key, budgets::PEER_REQUESTS)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the per-peer counter could not be reached");
            Refusal::Unavailable
        })? {
        crate::counter::Verdict::Admitted { .. } => Ok(()),
        crate::counter::Verdict::Limited { retry_after } => {
            tracing::info!(%peer, %retry_after, "a peer's events budget is spent");
            Err(Refusal::RateLimited { retry_after })
        }
    }
}

/// Revoke every live capability over `album` whose member is not on the roster `listed` names.
///
/// The one automatic revocation write (`S-E5`). A roster is the album owner's statement of who
/// may read it; a capability minted for a member the owner has just removed is a grant the
/// owner has withdrawn, and the peer holding it learns so from
/// `/.well-known/capsule/revoked-jti` rather than from a refusal it cannot explain.
///
/// **An epoch bump alone revokes nothing.** A member still on the roster still holds their keys
/// and the server has nothing to cut; the grant's own epoch binding, re-checked at every
/// presentation, is what handles a member who *left and came back*.
///
/// **A takedown revokes nothing either.** A moderation hold is a per-asset serving constraint
/// answering `410`, not a statement about who may read the album (design/moderation.md).
///
/// # Errors
///
/// Returns the store error. The caller — the roster route — logs it and still answers the
/// roster's own success: the roster is the fact, and a capability whose member has gone is
/// refused at its next presentation anyway, because membership is re-checked there. The gap is
/// bounded by the token's TTL and closes at the next revocation write.
pub async fn on_roster_applied(
    federation: &FederationContext,
    album: &crate::store::AlbumId,
    listed: &[crate::store::UserId],
) -> Result<usize, crate::store::StoreError> {
    let now = federation.clock().now();
    let live = federation
        .capabilities()
        .live(&CapabilityFilter::Album(album.clone()), now)
        .await?;
    let mut revoked = 0;
    for record in live {
        if listed.contains(&record.member) {
            continue;
        }
        federation
            .capabilities()
            .revoke_issued(&record.jti, now)
            .await?;
        revoked += 1;
        tracing::info!(
            %album,
            peer = %record.peer_id,
            member = %record.member,
            jti = %record.jti,
            "a roster change revoked a federation capability"
        );
    }
    if revoked > 0 {
        tracing::info!(%album, revoked, "a roster change cut federated grants");
    }
    Ok(revoked)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::counter::{Budget, CounterStore, InMemoryCounters, Verdict};
    use crate::store::memory::ManualClock;
    use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

    #[test]
    fn a_peer_id_is_canonical() {
        assert_eq!(PeerId::new("Other.Test."), PeerId::new("other.test"));
        assert_eq!(PeerId::new(" other.test ").as_str(), "other.test");
        assert_ne!(PeerId::new("other.test"), PeerId::new("another.test"));
    }

    /// A counter that cannot be reached.
    #[derive(Debug)]
    struct DownCounters;

    fn down<T>() -> StoreFuture<'static, T> {
        Box::pin(async {
            Err(StoreError::Unavailable {
                store: "counters",
                detail: "down".to_owned(),
            })
        })
    }

    impl CounterStore for DownCounters {
        fn hit<'a>(
            &'a self,
            _: &'a CounterKey,
            _: Budget,
            _: Timestamp,
        ) -> StoreFuture<'a, Verdict> {
            down()
        }

        fn peek<'a>(
            &'a self,
            _: &'a CounterKey,
            _: Budget,
            _: Timestamp,
        ) -> StoreFuture<'a, Verdict> {
            down()
        }

        fn reset<'a>(&'a self, _: &'a CounterKey) -> StoreFuture<'a, ()> {
            down()
        }
    }

    /// A peer store that cannot be reached.
    #[derive(Debug)]
    struct DownPeers;

    impl PeerStore for DownPeers {
        fn pin<'a>(&'a self, _: &'a PeerId, _: [u8; 32], _: Timestamp) -> StoreFuture<'a, ()> {
            down()
        }

        fn read<'a>(&'a self, _: &'a PeerId) -> StoreFuture<'a, Option<PeerRecord>> {
            down()
        }

        fn block<'a>(
            &'a self,
            _: &'a PeerId,
            _: Timestamp,
            _: Option<String>,
        ) -> StoreFuture<'a, BlockOutcome> {
            down()
        }

        fn unblock<'a>(&'a self, _: &'a PeerId) -> StoreFuture<'a, UnblockOutcome> {
            down()
        }
    }

    fn context(peers: Arc<dyn PeerStore>, clock: Arc<ManualClock>) -> FederationContext {
        let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .expect("a key generates");
        FederationContext::new(FederationCollaborators {
            codec: Arc::new(
                CapabilityCodec::from_pkcs8(der.as_ref(), "home.test", clock.clone())
                    .expect("parses"),
            ),
            capabilities: Arc::new(InMemoryCapabilities::new(clock.clone())),
            peers,
            clock,
            federation_url: None,
        })
    }

    fn verified(clock: &ManualClock) -> VerifiedCapability {
        let now = clock.now();
        let record = CapabilityRecord {
            jti: "01937b7c-0000-7000-8000-0000000000aa".to_owned(),
            album_id: AlbumId::new("album"),
            peer_id: PeerId::new("other.test"),
            member: UserId::new("bob"),
            scope: Scope::Read,
            granted_epoch: 1,
            min_protocol_version: "2026-06-01".to_owned(),
            issued_at: now,
            expires_at: crate::store::deadline(now, SignedDuration::from_hours(1)),
            revoked_at: None,
            refreshed_to: None,
        };
        VerifiedCapability {
            grant: record.grant(),
            record,
        }
    }

    #[tokio::test]
    async fn a_predecessor_revoked_by_its_own_refresh_is_admitted_only_to_be_refreshed() {
        // What makes a replayed refresh answerable: the predecessor is revoked the moment its
        // successor is issued, and a rule that refused every revoked grant would make the
        // idempotency the contract promises unreachable. A read is still refused.
        let clock = Arc::new(ManualClock::default());
        let mut capability = verified(&clock);
        capability.record.revoked_at = Some(clock.now());
        capability.record.refreshed_to = Some("01937b7c-0000-7000-8000-0000000000bb".to_owned());
        let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
        let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock);
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Refresh).await,
            Ok(())
        );
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Read).await,
            Err(Refusal::Revoked)
        );

        // A grant revoked without a successor is refused on both.
        capability.record.refreshed_to = None;
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Refresh).await,
            Err(Refusal::Revoked)
        );
    }

    #[tokio::test]
    async fn a_store_that_cannot_answer_an_admission_is_an_outage_never_an_admission() {
        // The fail-closed rule at the seam every federated read passes through: a peer store
        // or a counter that cannot be reached decides nothing, and "nothing" is a refusal.
        let clock = Arc::new(ManualClock::default());
        let capability = verified(&clock);

        let federation = context(Arc::new(DownPeers), clock.clone());
        let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone());
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Read).await,
            Err(Refusal::Unavailable)
        );

        let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
        let counters = CounterContext::new(Arc::new(DownCounters), clock.clone());
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Read).await,
            Err(Refusal::Unavailable)
        );

        let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock);
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Read).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn a_revoked_capability_is_refused_before_the_peer_is_charged() {
        let clock = Arc::new(ManualClock::default());
        let mut capability = verified(&clock);
        capability.record.revoked_at = Some(clock.now());
        let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
        let store = Arc::new(InMemoryCounters::new());
        let counters = CounterContext::new(store.clone(), clock.clone());
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Read).await,
            Err(Refusal::Revoked)
        );
        assert_eq!(
            store
                .peek(
                    &CounterKey::PeerRequests("other.test".to_owned()),
                    budgets::PEER_REQUESTS,
                    clock.now(),
                )
                .await
                .expect("answers"),
            Verdict::Admitted {
                remaining: budgets::PEER_REQUESTS.limit
            },
            "a revoked grant costs the peer nothing"
        );
    }
}
