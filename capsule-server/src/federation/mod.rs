//! Server-to-server federation: the capability that gates which peer
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
//!   blocklist, which is a column on the same row; and [`PeerAdministration`], the operator's
//!   `capsule-server peer …` acts on it, a block's cascade over the issued grants included.
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
pub mod postgres;
pub mod report;
pub mod scheme;
pub mod store;

pub use self::capability::{
    ALBUM_URN_PREFIX, CapabilityCodec, CapabilityError, CapabilityGrant, MintError, MintRequest,
    Minted, Scope, album_from_urn, album_urn,
};
pub use self::memory::{InMemoryCapabilities, InMemoryPeers};
pub use self::peers::{
    BlockOutcome, Blocked, InvalidSigningKey, PeerAdministration, PeerRecord, PeerStore,
    PinOutcome, UnblockOutcome, parse_signing_key, revoke_grants_of,
};
pub use self::report::{
    REPORT_FRESHNESS, REPORT_NONCE_WINDOW, ReportClaim, ReportError, verify_signed_report,
};
pub use self::scheme::{Principal, ReadBearer, VerifiedCapability};
pub use self::store::{
    CapabilityFilter, CapabilityRecord, CapabilityStore, MAX_GRANT_LIFETIME, RefreshOutcome,
    RevokeOutcome,
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

    /// Wraps an origin a caller sent, refusing anything that is not a host name.
    ///
    /// [`Self::new`] folds and never refuses, because it also wraps values this server already
    /// holds. This is the door for values a caller chose — the peer a mint names, the
    /// `reporting_server` a report claims — and every one of them becomes a store row, a counter
    /// key and a log field. So the grammar is checked before any of that: after folding, at most
    /// [`MAX_PEER_ID_BYTES`], one or more dot-separated labels of 1 to 63 ASCII letters, digits
    /// and hyphens, no label starting or ending with a hyphen (RFC 1035's preferred syntax, with
    /// RFC 1123's leading digit). No port, no scheme, no path, no userinfo: a peer is named the
    /// way its own `server-info` names itself.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPeerId`] when the folded value is not such a name.
    pub fn parse(value: &str) -> Result<Self, InvalidPeerId> {
        let peer = Self::new(value);
        let host = peer.as_str();
        let label_ok = |label: &str| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        };
        if host.is_empty() || host.len() > MAX_PEER_ID_BYTES || !host.split('.').all(label_ok) {
            return Err(InvalidPeerId);
        }
        Ok(peer)
    }

    /// The origin as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The longest peer origin [`PeerId::parse`] accepts: RFC 1035's ceiling on a domain name.
pub const MAX_PEER_ID_BYTES: usize = 253;

/// A caller-supplied peer origin that is not a host name. See [`PeerId::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a peer origin must be a host name")]
pub struct InvalidPeerId;

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
    /// Whether this deployment federates at all (`FEDERATION_ENABLED`, #509).
    ///
    /// Off is the default, and off means the federation surface is not there: the four
    /// federation operations answer `404` before any credential, budget or store is touched,
    /// and [`ReadBearer`] refuses every capability on the two read primitives. See
    /// [`FederationSetting`] for why this is a port rather than a `bool`.
    pub enabled: Arc<dyn FederationSetting>,
}

/// Whether this deployment federates (`FEDERATION_ENABLED`, #509).
///
/// Federation is deferred (#476), and a deployment that has not asked for it must not carry its
/// unauthenticated surface: report intake and minting were both reachable in `serve` before
/// this gate, and both do store work for whoever asks. So federation is **off unless an operator
/// turns it on**, and off is enforced at the two places a federated request can enter — the
/// gate every federation operation extracts first, and the capability arm of [`ReadBearer`].
///
/// # Why a port and not a `bool`
///
/// Production reads the setting once at boot and never changes it: `boot::assemble` hands in a
/// plain `bool`, which implements this trait. It is a trait only because the conformance walk
/// (`tests/conformance.rs`) proves every response the document declares through **one** server,
/// and the `404` this gate declares has to be produced by the same server that produces every
/// federated `200` — so the test double is a switch, the way the suite's other doubles are.
pub trait FederationSetting: fmt::Debug + Send + Sync {
    /// Whether federated requests are served at all.
    fn is_enabled(&self) -> bool;
}

/// The setting as boot reads it: fixed for the life of the process.
impl FederationSetting for bool {
    fn is_enabled(&self) -> bool {
        *self
    }
}

/// The federation module's collaborators.
#[derive(Debug, Clone)]
pub struct FederationContext {
    codec: Arc<CapabilityCodec>,
    capabilities: Arc<dyn CapabilityStore>,
    peers: Arc<dyn PeerStore>,
    clock: Arc<dyn Clock>,
    enabled: Arc<dyn FederationSetting>,
}

impl FederationContext {
    /// Assembles the module.
    pub fn new(collaborators: FederationCollaborators) -> Self {
        let FederationCollaborators {
            codec,
            capabilities,
            peers,
            clock,
            enabled,
        } = collaborators;
        Self {
            codec,
            capabilities,
            peers,
            clock,
            enabled,
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

    /// Whether this deployment federates at all.
    ///
    /// The gate on every federated request: the four federation operations and the capability
    /// arm of [`ReadBearer`]. A capability minted while federation was on is not un-minted by
    /// turning it off, but it is not honoured either — nothing federated is served while the
    /// surface is off, which is what makes "off" a statement an operator can rely on.
    pub fn is_enabled(&self) -> bool {
        self.enabled.is_enabled()
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
    /// The peer's events-per-hour or bytes-per-hour budget is spent.
    /// `429 error.federation.rate_budget_exceeded`.
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

/// Which budgets a peer is held to: design/federation.md's probation tier, on the serving side.
///
/// Decided from [`PeerRecord::first_seen_at`] against [`budgets::PEER_PROBATION`], at the
/// instant of the request, by [`PeerTier::of`] — the one place both the authenticator (which
/// foresees a spent budget) and [`admit`] (which charges it) read it, so the two cannot hold a
/// peer to different numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTier {
    /// Known to this server for less than [`budgets::PEER_PROBATION`], or not known at all.
    Probation,
    /// Known for longer.
    Established,
}

impl PeerTier {
    /// The tier `record` puts its peer in at `now`.
    ///
    /// No record is probation: a peer this server has never recorded has not behaved yet.
    pub fn of(record: Option<&PeerRecord>, now: jiff::Timestamp) -> Self {
        match record {
            Some(record)
                if crate::store::deadline(record.first_seen_at, budgets::PEER_PROBATION) <= now =>
            {
                Self::Established
            }
            _ => Self::Probation,
        }
    }

    /// The events-per-hour budget for this tier.
    pub fn requests(self) -> crate::counter::Budget {
        match self {
            Self::Probation => budgets::PEER_REQUESTS_PROBATION,
            Self::Established => budgets::PEER_REQUESTS,
        }
    }

    /// The kibibytes-per-hour budget for this tier.
    pub fn kibibytes(self) -> crate::counter::Budget {
        match self {
            Self::Probation => budgets::PEER_KIBIBYTES_PROBATION,
            Self::Established => budgets::PEER_KIBIBYTES,
        }
    }

    /// The name this tier travels under, for a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probation => "probation",
            Self::Established => "established",
        }
    }
}

/// Decide whether `capability` may be presented at all, and charge the peer's budget if so.
///
/// The three questions every federated request asks before it looks at what is being asked for:
/// is the grant still live, is the peer still welcome, and is the peer within budget. Asked
/// here once so the sync, blob and refresh routes cannot ask them in different orders.
///
/// Answers the [`PeerTier`] it charged under, so a route that charges a second budget — the
/// blob route's bytes — holds the peer to the same tier without reading the peer store again.
///
/// # Errors
///
/// Returns the [`Refusal`] the route renders.
pub async fn admit(
    federation: &FederationContext,
    counters: &CounterContext,
    capability: &VerifiedCapability,
    presentation: Presentation,
) -> Result<PeerTier, Refusal> {
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

    let record = federation.peers().read(peer).await.map_err(|error| {
        tracing::error!(%error, %peer, "the peer store could not answer an admission");
        Refusal::Unavailable
    })?;
    if record.as_ref().is_some_and(PeerRecord::is_blocked) {
        tracing::info!(%peer, "a blocked peer presented a capability");
        return Err(Refusal::PeerBlocked);
    }

    // The authenticator left this request's nonce unspent because it foresaw this refusal. An
    // unblock or a budget window rolling over since then does not admit it: its signature was
    // never recorded, so admitting it would let the same signature be presented again.
    if let Some(refusal) = capability.foreseen {
        tracing::info!(
            %peer,
            ?refusal,
            "a request whose nonce was left unspent is refused as the authenticator foresaw"
        );
        return Err(refusal);
    }

    let tier = PeerTier::of(record.as_ref(), federation.clock().now());
    let key = CounterKey::PeerRequests(peer.as_str().to_owned());
    match counters.hit(&key, tier.requests()).await.map_err(|error| {
        tracing::error!(%error, %peer, "the per-peer counter could not be reached");
        Refusal::Unavailable
    })? {
        crate::counter::Verdict::Admitted { .. } => Ok(tier),
        crate::counter::Verdict::Limited { retry_after } => {
            tracing::info!(%peer, tier = tier.as_str(), %retry_after, "a peer's events budget is spent");
            Err(Refusal::RateLimited { retry_after })
        }
    }
}

/// How many kibibytes a delivery of `bytes` is charged: rounded up, so no delivery is free.
pub fn kibibytes(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(1024)).unwrap_or(u32::MAX)
}

/// Charge `bytes` served to `peer` against its bytes-per-hour budget, in one weighted call.
///
/// The blob route calls this once it knows what the delivery will send, and before it sends
/// it: a refusal here is a `429` in place of the body, never a body cut off part-way. The whole
/// blob is charged for a ranged delivery too — the range is not visible to the route, and the
/// blob's size is an upper bound on what any range of it sends, so the budget errs toward the
/// peer having spent more than it did and never less.
///
/// # Errors
///
/// [`Refusal::RateLimited`] when what is left of the window cannot take the charge, and
/// [`Refusal::Unavailable`] when the counter cannot answer — a refusal, never an admission.
pub async fn charge_served_bytes(
    counters: &CounterContext,
    peer: &PeerId,
    tier: PeerTier,
    bytes: u64,
) -> Result<(), Refusal> {
    let key = CounterKey::PeerBytes(peer.as_str().to_owned());
    let units = kibibytes(bytes);
    match counters
        .charge(&key, units, tier.kibibytes())
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the per-peer bytes counter could not be reached");
            Refusal::Unavailable
        })? {
        crate::counter::Verdict::Admitted { remaining } => {
            tracing::debug!(%peer, tier = tier.as_str(), kibibytes = units, remaining, "charged a peer for bytes served");
            Ok(())
        }
        crate::counter::Verdict::Limited { retry_after } => {
            tracing::info!(%peer, tier = tier.as_str(), kibibytes = units, %retry_after, "a peer's bytes budget is spent");
            Err(Refusal::RateLimited { retry_after })
        }
    }
}

/// Revoke every live capability over `album` whose member is not on the roster `listed` names.
///
/// The one automatic revocation write. A roster is the album owner's statement of who
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
mod tests;
