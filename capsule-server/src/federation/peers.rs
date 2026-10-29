//! [`PeerStore`] — the peer servers this one knows: their pinned signing keys, and the
//! server-level blocklist.
//!
//! # Operator-pinned, not fetched
//!
//! design/federation.md describes peers caching each other's keys TOFU-style with a perspective
//! check on rotation. This server has no outbound HTTP client at all — nothing in
//! `capsule-server` reaches out to another server — so in v1 a peer's key arrives the way a
//! deployment's own key does: an operator puts it there. That is stated rather than worked
//! around because the alternative, fetching `server-info` at report intake, would make the
//! first federated report from a new peer the thing that decides whether it is trusted.
//!
//! **Minting needs no peer key.** A capability is signed with this server's own key, and the
//! peer verifies it against `server-info`. The pinned key serves two things, both of them
//! "did this peer say so": the request signature every capability presentation carries
//! ([`ReadBearer`](super::ReadBearer)), which is what makes a leaked capability useless to
//! anyone but its peer, and the signature on a federated moderation report. A peer with no
//! pinned key can therefore pull nothing, whatever capability it holds.
//!
//! # The blocklist is a column
//!
//! design/moderation.md's server-level blocklist "operates at the federation capability layer",
//! and here it is a row's `blocked_at`. Blocking a peer nobody has pinned is legitimate — an
//! operator blocks a server they never wanted to hear from — so a block creates the row without
//! a key. Every federation boundary consults it: mint, presentation, refresh, report intake.

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jiff::Timestamp;

use super::PeerId;
use super::store::{CapabilityFilter, CapabilityStore};
use crate::store::{Clock, StoreError, StoreFuture};

/// What this server knows about one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRecord {
    /// The peer's canonical origin.
    pub server_id: PeerId,
    /// Its operational Ed25519 public key, if an operator has pinned one.
    pub signing_key: Option<[u8; 32]>,
    /// When this server first recorded the peer, by a pin or by a block.
    pub first_seen_at: Timestamp,
    /// When it was blocked, while it is.
    pub blocked_at: Option<Timestamp>,
    /// The operator's note on the block, if they left one.
    pub note: Option<String>,
}

impl PeerRecord {
    /// Whether federated requests from this peer are refused.
    pub fn is_blocked(&self) -> bool {
        self.blocked_at.is_some()
    }
}

/// What blocking did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockOutcome {
    /// The peer is now blocked.
    Blocked,
    /// It already was. A retry is not a new fact.
    AlreadyBlocked,
}

/// What unblocking did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnblockOutcome {
    /// The peer is no longer blocked.
    Unblocked,
    /// It was not blocked, or was never recorded.
    NotBlocked,
}

/// Where peers are kept.
pub trait PeerStore: fmt::Debug + Send + Sync {
    /// Pin `signing_key` as `peer`'s operational key, at `at`.
    ///
    /// Replaces a key already pinned: rotation is an operator act here. A block already on
    /// the row is kept — pinning a key is not an opinion about whether to talk to its owner.
    fn pin<'a>(
        &'a self,
        peer: &'a PeerId,
        signing_key: [u8; 32],
        at: Timestamp,
    ) -> StoreFuture<'a, ()>;

    /// What is known about `peer`, if anything.
    fn read<'a>(&'a self, peer: &'a PeerId) -> StoreFuture<'a, Option<PeerRecord>>;

    /// Refuse federated requests from `peer` from `at`, with `note` for the operator's record.
    ///
    /// Creates the row if the peer was never pinned. Idempotent.
    fn block<'a>(
        &'a self,
        peer: &'a PeerId,
        at: Timestamp,
        note: Option<String>,
    ) -> StoreFuture<'a, BlockOutcome>;

    /// Lift a block on `peer`. The pinned key, if any, is kept.
    fn unblock<'a>(&'a self, peer: &'a PeerId) -> StoreFuture<'a, UnblockOutcome>;
}

/// What pinning did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// The peer had no key, and now has this one.
    Pinned,
    /// The peer had another key, and this one replaces it: an operator-performed rotation.
    Rotated,
    /// The peer already had exactly this key. A retry is not a new fact.
    Unchanged,
}

/// What blocking did, with the cascade that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocked {
    /// Whether the block is new.
    pub outcome: BlockOutcome,
    /// How many live capabilities the cascade revoked and published.
    pub revoked: usize,
}

/// A signing key that is not the base64 of a raw 32-byte Ed25519 public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a peer signing key must be the base64 of a raw 32-byte Ed25519 public key")]
pub struct InvalidSigningKey;

/// Read a peer's signing key in the form its `server-info` publishes `signing_key`: the raw
/// Ed25519 public key, standard base64 with padding.
///
/// The same spelling on both ends, so the operator's act is to copy the field the peer's own
/// discovery document shows — the out-of-band step that makes the pin a decision rather than
/// a fetch (see the module docs).
///
/// # Errors
///
/// Returns [`InvalidSigningKey`] for anything that is not exactly that.
pub fn parse_signing_key(text: &str) -> Result<[u8; 32], InvalidSigningKey> {
    let bytes = BASE64.decode(text.trim()).map_err(|_| InvalidSigningKey)?;
    bytes.try_into().map_err(|_| InvalidSigningKey)
}

/// The operator's acts on the peer list (#476): pin a key, block, unblock, read.
///
/// What `capsule-server peer …` drives, over the **same** peer and capability stores the server
/// reads, so a key pinned here is the key the next presentation verifies against and a block
/// written here is refused at the next boundary. It holds no signing key: none of these acts
/// signs anything, and the maintenance assembly that builds it has none in scope.
///
/// # Blocking cascades, every time
///
/// [`Self::block`] writes the block and then revokes and publishes every live capability the
/// peer holds. The block enforces itself the moment it is written — `blocked_at` is consulted at
/// mint, presentation, refresh and report intake — and the cascade adds **publication**: the
/// peer's `jti`s reach `/.well-known/capsule/revoked-jti`, and an operator who later unblocks the
/// peer does not silently restore access the block was meant to end.
///
/// The cascade runs on a repeated block too. A first block whose cascade failed part-way has
/// already refused every request; running the command again is then how the operator finishes
/// the publication, and there is no state in which "already blocked" means "already cut".
#[derive(Debug, Clone)]
pub struct PeerAdministration {
    peers: Arc<dyn PeerStore>,
    capabilities: Arc<dyn CapabilityStore>,
    clock: Arc<dyn Clock>,
}

impl PeerAdministration {
    /// Acts on `peers`, cascading blocks over `capabilities`, stamped from `clock`.
    pub fn new(
        peers: Arc<dyn PeerStore>,
        capabilities: Arc<dyn CapabilityStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            peers,
            capabilities,
            clock,
        }
    }

    /// Pin `signing_key` as `peer`'s operational key.
    ///
    /// The outcome is read before the write and is for the operator's report only: the write
    /// itself is [`PeerStore::pin`], which replaces whatever was there.
    ///
    /// # Errors
    ///
    /// Returns the store error.
    pub async fn pin(
        &self,
        peer: &PeerId,
        signing_key: [u8; 32],
    ) -> Result<PinOutcome, StoreError> {
        let before = self
            .peers
            .read(peer)
            .await?
            .and_then(|record| record.signing_key);
        let outcome = match before {
            None => PinOutcome::Pinned,
            Some(held) if held == signing_key => PinOutcome::Unchanged,
            Some(_) => PinOutcome::Rotated,
        };
        if outcome != PinOutcome::Unchanged {
            self.peers.pin(peer, signing_key, self.clock.now()).await?;
        }
        tracing::info!(%peer, ?outcome, "an operator pinned a peer's signing key");
        Ok(outcome)
    }

    /// Block `peer`, with `note` for the record, and cut every live grant it holds.
    ///
    /// # Errors
    ///
    /// Returns the store error. A failure after the block was written leaves the block in force
    /// — every request from the peer is refused — and the operator runs the command again to
    /// finish the cascade.
    pub async fn block(&self, peer: &PeerId, note: Option<String>) -> Result<Blocked, StoreError> {
        let now = self.clock.now();
        let outcome = self.peers.block(peer, now, note).await?;
        tracing::info!(%peer, ?outcome, "an operator blocked a peer");
        let revoked = revoke_grants_of(self.capabilities.as_ref(), peer, now).await?;
        Ok(Blocked { outcome, revoked })
    }

    /// Lift a block on `peer`. Revoked grants stay revoked: unblocking restores nothing.
    ///
    /// # Errors
    ///
    /// Returns the store error.
    pub async fn unblock(&self, peer: &PeerId) -> Result<UnblockOutcome, StoreError> {
        let outcome = self.peers.unblock(peer).await?;
        tracing::info!(%peer, ?outcome, "an operator unblocked a peer");
        Ok(outcome)
    }

    /// What is known about `peer`, if anything.
    ///
    /// # Errors
    ///
    /// Returns the store error.
    pub async fn read(&self, peer: &PeerId) -> Result<Option<PeerRecord>, StoreError> {
        self.peers.read(peer).await
    }
}

/// Revoke and publish every capability `peer` holds that is live at `now`.
///
/// Each through [`CapabilityStore::revoke_issued`], so a grant's refresh successors are cut with
/// it. Returns how many were revoked.
///
/// # Errors
///
/// Returns the store error; what was revoked before it stays revoked.
pub async fn revoke_grants_of(
    capabilities: &dyn CapabilityStore,
    peer: &PeerId,
    now: Timestamp,
) -> Result<usize, StoreError> {
    let live = capabilities
        .live(&CapabilityFilter::Peer(peer.clone()), now)
        .await?;
    let mut revoked = 0;
    for record in live {
        capabilities.revoke_issued(&record.jti, now).await?;
        revoked += 1;
        tracing::debug!(%peer, jti = %record.jti, "a blocked peer's grant was revoked");
    }
    tracing::info!(%peer, revoked, "a peer was blocked and its grants were cut");
    Ok(revoked)
}

#[cfg(test)]
mod tests;
