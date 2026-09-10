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
//! peer verifies it against `server-info`. The pinned key serves exactly one thing: verifying
//! the signature on a federated moderation report.
//!
//! # The blocklist is a column
//!
//! design/moderation.md's server-level blocklist "operates at the federation capability layer",
//! and here it is a row's `blocked_at`. Blocking a peer nobody has pinned is legitimate — an
//! operator blocks a server they never wanted to hear from — so a block creates the row without
//! a key. Every federation boundary consults it: mint, presentation, refresh, report intake.

use std::fmt;

use jiff::Timestamp;

use super::PeerId;
use crate::store::StoreFuture;

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
