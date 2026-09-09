//! [`CapabilityStore`] — every capability this server issued, and the revocation list it
//! publishes.
//!
//! # The store is the revocation list
//!
//! `/.well-known/capsule/revoked-jti` was served from a standalone list before federation had a
//! minting side (`S-C18`). Once a capability is a stored record, "is this `jti` revoked" has a
//! second possible answer — the record's `revoked_at` — and two answers to that question is the
//! one shape revocation cannot afford. So every adapter here **is** a
//! [`RevocationList`]: revoking an issued capability sets its `revoked_at` and publishes its
//! `jti` in one critical section, and the standalone in-memory list is gone.
//!
//! [`RevocationList::revoke`] still accepts a `jti` this server never issued, and still
//! publishes it: an operator revoking a token by hand from a peer's report, or a record that
//! predates the store, is a fact the list must carry whether or not a row backs it.
//!
//! # What the record binds that the token does not
//!
//! The token names the peer, the album and the scope. The record adds the **member** the
//! capability was minted for and the **epoch** their membership was granted at
//! ([`CapabilityRecord::granted_epoch`]), so presentation can ask whether that member is still
//! on the roster at that epoch: a member removed and re-admitted later gets a fresh grant, and
//! the old capability — minted for a membership that ended — is refused without anyone having
//! revoked it. The token format is normative and parsed by every peer, which is why the epoch is
//! a stored fact rather than a claim.
//!
//! # Refresh is one operation
//!
//! [`CapabilityStore::refresh`] issues the successor, marks the predecessor as refreshed *to*
//! it, and revokes the predecessor, in one critical section. Idempotency keyed by
//! `(peer, jti)` — threat-model/validation.md — falls out of the `refreshed_to` link: a replay
//! finds the predecessor already refreshed and answers with the same successor, and two
//! concurrent refreshes of one token cannot both issue. The `peer` half of the key is the
//! credential's: only the holder of the predecessor can present it, and the store refuses a
//! successor that names another peer, album or member than the predecessor did, so the link
//! can never widen what was granted. A successor answered to a replay may itself have been
//! revoked since (a block cascades over every live capability of a peer); the route re-checks
//! [`CapabilityRecord::is_live`] before re-signing it.

use std::fmt;

use jiff::Timestamp;

use super::PeerId;
use super::capability::{CapabilityGrant, Scope};
use crate::discovery::revocation::{MAX_TOKEN_TTL, RevocationList};
use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

/// One capability this server issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRecord {
    /// The token's `jti`, and the revocation key.
    pub jti: String,
    /// The album it scopes to.
    pub album_id: AlbumId,
    /// The peer server it was issued to.
    pub peer_id: PeerId,
    /// The roster member whose access it carries, as the owner listed them.
    pub member: UserId,
    /// What it permits.
    pub scope: Scope,
    /// The epoch the member's membership was granted at when this was minted.
    pub granted_epoch: u64,
    /// The album's pinned protocol date, carried so the grant can be re-signed.
    pub min_protocol_version: String,
    /// When it was minted; also its `nbf`.
    pub issued_at: Timestamp,
    /// When it stops being honoured.
    pub expires_at: Timestamp,
    /// The absolute deadline the **whole grant** dies at, chosen at the original mint.
    ///
    /// The token's own `expires_at` is at most 24 h out and a refresh replaces it; this is the
    /// thing a refresh cannot move. Equal to `expires_at` for a grant the owner did not make
    /// renewable, which is the default — see [`CapabilityRecord::may_refresh_at`].
    pub not_after: Timestamp,
    /// When it was revoked, if it has been.
    pub revoked_at: Option<Timestamp>,
    /// The `jti` of the successor a refresh issued, if one has.
    pub refreshed_to: Option<String>,
}

impl CapabilityRecord {
    /// Whether the capability may still be presented at `now`: unrevoked and unexpired.
    pub fn is_live(&self, now: Timestamp) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }

    /// Whether a successor may still be issued from this grant at `now`.
    ///
    /// The absolute deadline, and the whole of what stops a refresh chain. Without it a peer
    /// holding a deliberate sixty-second grant refreshes inside the minute to the default TTL
    /// and again forever, and the owner's chosen lifetime is advisory for exactly one hop.
    ///
    /// Two conditions, and the first is the default: the owner must have made the grant
    /// renewable at all, and the deadline must not have passed. The last token of a renewable
    /// grant is minted for exactly what is left, so it satisfies neither and answers the same
    /// "this grant is over" as one that was never renewable — which is the honest answer in
    /// both cases, because in both there is no successor left to have.
    pub fn may_refresh_at(&self, now: Timestamp) -> bool {
        self.is_renewable() && self.not_after > now
    }

    /// Whether the owner made this grant renewable at all.
    ///
    /// `false` is the default: renewability is asked for at mint, never assumed.
    pub fn is_renewable(&self) -> bool {
        self.not_after > self.expires_at
    }

    /// The grant this record describes, which the codec re-signs byte-for-byte.
    pub fn grant(&self) -> CapabilityGrant {
        CapabilityGrant {
            peer: self.peer_id.clone(),
            album: self.album_id.clone(),
            scope: self.scope,
            jti: self.jti.clone(),
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            min_protocol_version: self.min_protocol_version.clone(),
        }
    }
}

/// Refuse a record whose lifetime the published list could not stay bounded under.
///
/// Shared by every adapter rather than re-derived in each: the 24 h ceiling and the absolute
/// deadline are properties of the *record*, and an adapter that checked them differently would
/// be an adapter that accepted a grant another one refuses.
///
/// # Errors
///
/// Returns [`StoreError::Rejected`](crate::store::StoreError::Rejected) when the token's own
/// window is past [`MAX_TOKEN_TTL`], or when it runs past the grant's absolute deadline.
pub fn admissible(record: &CapabilityRecord) -> Result<(), StoreError> {
    if record.expires_at.duration_since(record.issued_at) > MAX_TOKEN_TTL {
        return Err(StoreError::Rejected {
            store: "capabilities",
            detail: format!(
                "capability {} would live past the {MAX_TOKEN_TTL} ceiling",
                record.jti
            ),
        });
    }
    if record.expires_at > record.not_after {
        return Err(StoreError::Rejected {
            store: "capabilities",
            detail: format!(
                "capability {} expires at {}, past its grant's deadline of {}",
                record.jti, record.expires_at, record.not_after
            ),
        });
    }
    Ok(())
}

/// Refuse a successor that does not continue exactly what its predecessor granted.
///
/// The four things a refresh may never move: the peer, the album, the member, and the absolute
/// deadline. The first three keep a refresh from *widening* a grant; the fourth keeps it from
/// *outliving* one, which is the same defect one dimension along.
///
/// # Errors
///
/// Returns [`StoreError::Rejected`](crate::store::StoreError::Rejected) naming which of them
/// moved. Every one is a bug in the caller, never a peer's request.
pub fn continues(
    predecessor: &str,
    old: &CapabilityRecord,
    successor: &CapabilityRecord,
) -> Result<(), StoreError> {
    if successor.peer_id != old.peer_id
        || successor.album_id != old.album_id
        || successor.member != old.member
    {
        return Err(StoreError::Rejected {
            store: "capabilities",
            detail: format!("a successor of {predecessor} must carry its peer, album and member"),
        });
    }
    if successor.not_after != old.not_after {
        return Err(StoreError::Rejected {
            store: "capabilities",
            detail: format!(
                "a successor of {predecessor} must carry its deadline of {}, not {}",
                old.not_after, successor.not_after
            ),
        });
    }
    Ok(())
}

/// Which live capabilities a caller wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityFilter {
    /// Every live capability over one album — what a roster change consults.
    Album(AlbumId),
    /// Every live capability held by one peer — what a block cascades over.
    Peer(PeerId),
}

/// What revoking an issued capability did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// It was live and is now revoked and published.
    Revoked,
    /// It was already revoked. A retry is not a new fact.
    AlreadyRevoked,
    /// No capability with that `jti` was ever issued here.
    Unknown,
}

/// What a refresh did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The successor was issued and the predecessor revoked and linked to it.
    Issued(CapabilityRecord),
    /// The predecessor had already been refreshed; this is the successor it links to.
    AlreadyRefreshed(CapabilityRecord),
    /// The predecessor was revoked without a successor, so there is nothing to continue.
    Revoked,
    /// No capability with the predecessor's `jti` was ever issued here.
    Unknown,
}

/// Where issued capabilities live, and the revocation list they feed.
pub trait CapabilityStore: RevocationList + fmt::Debug + Send + Sync {
    /// Record a freshly minted capability.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Rejected`](crate::store::StoreError::Rejected) if a capability with
    /// the same `jti` is already recorded — a `jti` is a fresh UUIDv7 per mint, so a collision is
    /// a bug rather than a retry — or if the record would live past the TTL ceiling, which the
    /// published list is bounded by. The codec clamps at mint, so the second is a bug too.
    fn issue(&self, record: CapabilityRecord) -> StoreFuture<'_, ()>;

    /// The capability `jti` names, revoked or not.
    fn find<'a>(&'a self, jti: &'a str) -> StoreFuture<'a, Option<CapabilityRecord>>;

    /// Every capability matching `filter` that is live at `now`.
    fn live<'a>(
        &'a self,
        filter: &'a CapabilityFilter,
        now: Timestamp,
    ) -> StoreFuture<'a, Vec<CapabilityRecord>>;

    /// Revoke the capability `jti` names at `at`, and publish its `jti`, in one operation.
    ///
    /// Idempotent: a second call answers [`RevokeOutcome::AlreadyRevoked`] and changes nothing.
    fn revoke_issued<'a>(&'a self, jti: &'a str, at: Timestamp) -> StoreFuture<'a, RevokeOutcome>;

    /// Issue `successor` in place of the capability `predecessor` names, at `at`.
    ///
    /// One critical section: the successor is recorded, the predecessor's `refreshed_to` is set
    /// to it, and the predecessor is revoked and published. A predecessor that has already been
    /// refreshed answers [`RefreshOutcome::AlreadyRefreshed`] with the successor it links to and
    /// records nothing — which is the idempotency the contract promises.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Rejected`](crate::store::StoreError::Rejected) if `successor` names
    /// a different peer, album or member than the predecessor, **carries a different
    /// `not_after` or one its own `expires_at` runs past**, or would live past the ceiling, or
    /// reuses a recorded `jti`. Every one is a bug in the caller, never a peer's request — and
    /// the `not_after` rule is what makes "a refresh cannot extend a grant" structural rather
    /// than a property of the one route that happens to compute the successor's TTL.
    fn refresh<'a>(
        &'a self,
        predecessor: &'a str,
        successor: CapabilityRecord,
        at: Timestamp,
    ) -> StoreFuture<'a, RefreshOutcome>;
}
