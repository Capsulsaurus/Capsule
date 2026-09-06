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
pub mod store;

pub use self::capability::{
    ALBUM_URN_PREFIX, CapabilityCodec, CapabilityError, CapabilityGrant, MintError, MintRequest,
    Minted, Scope, album_from_urn, album_urn,
};
pub use self::memory::{InMemoryCapabilities, InMemoryPeers};
pub use self::peers::{BlockOutcome, PeerRecord, PeerStore, UnblockOutcome};
pub use self::store::{
    CapabilityFilter, CapabilityRecord, CapabilityStore, RefreshOutcome, RevokeOutcome,
};
use crate::store::Clock;

/// A peer server's identity: its canonical origin, as its own `server-info` publishes it.
///
/// Its own type rather than a `UserId` or a bare string so a peer can never be handed to a port
/// that expects an account, and so the log field that names one reads as what it is.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(String);

impl PeerId {
    /// Wraps an already-validated origin.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
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
