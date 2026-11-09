//! Album membership: the one fact the key-free server holds about who may read and
//! write a shared album, and the port it is held behind.
//!
//! # What the server knows, and where it learned it
//!
//! The server cannot read the MLS roster — every membership change is AEAD-protected under a
//! group key it never holds — so what it knows is what the album owner **told** it: a
//! [`SignedAlbumRoster`](capsule_core::crypto::membership::SignedAlbumRoster), verified against
//! the owner's published device directory before it reaches this port (the roster route). The
//! port stores the *consequence* of that document — who is a member, with what role, since
//! which version and epoch — and never re-verifies it: the same rule `album/mod.rs` records for
//! a quiescence, that verification happens once at the write and a stored fact is read as a
//! fact.
//!
//! # Removal is a stored fact, not a deleted row
//!
//! A member who vanishes from a later roster is not deleted; the row is marked with the version
//! and epoch at which they vanished. That is what makes `403 error.blob.access_revoked`
//! renderable at all: `serve/authority.rs` reserves the `403` for a caller the server can see
//! once **had** access, and everyone else gets the unknown-address `404`. Delete the row and
//! the former member is indistinguishable from a stranger, and the authorization-change signal
//! design/transfer/download.md requires is gone.
//!
//! # Removing a member reclaims nothing, by design
//!
//! A writer member's upload is filed under the album **owner**'s namespace and charged to the
//! **uploader** (`routes/upload.rs`: `owner_id` is the namespace, `upload_user_id` is billed).
//! Removing that member from a later roster changes neither fact. The asset stays in the
//! owner's album, and its bytes stay against the removed member's quota until the owner or a
//! current writer member deletes the asset and the refcount collector (`gc/mod.rs`,
//! `QuotaStore::release_attribution`) releases the attribution once the last reference to the
//! bytes is gone.
//!
//! That is the decision design/quota.md records under "Departed album members" (issue #473),
//! not a gap: removal is a key rotation, not a data event, so it re-attributes nothing, and
//! neither alternative survives the rule that one account's quota pressure must never delete
//! another account's photos. Making the owner inherit the bytes lets a member's uploads push the
//! owner over quota; reclaiming them from the member deletes photos the owner still holds and
//! reads. So this port records who may read and write and touches no ledger, and the ledger's
//! side of the decision is pinned by
//! `quota::conformance::a_contributors_attribution_survives_until_the_collector_releases_it`.
//!
//! # One critical section
//!
//! [`MembershipStore::apply_roster`] compares versions and replaces the roster in **one**
//! operation, the way the device directory's `publish` does: two concurrent publishes cannot
//! both read "version 1 is current" and both write version 2. The in-memory adapter holds one
//! mutex; the Postgres adapter takes a per-album transaction lock.
//!
//! # The version is bounded above as well as below
//!
//! Monotonicity alone makes `roster_version` a one-way ratchet with no stop: a single publish at
//! the top of the counter can never be superseded, and the album's membership is frozen for
//! good. [`MAX_ROSTER_VERSION_STEP`] closes that — a roster is applied only inside a window
//! above the held version — and the refusal ([`RosterOutcome::VersionLeap`]) names the held
//! version, so the client re-signs at `held+1` and loses nothing: the roster is a full document,
//! so the version is only ever an ordering, never a count of anything.
//!
//! The window is clamped by [`MAX_ROSTER_VERSION`] as well as by the step, which is what keeps
//! the counter inside what a `BIGINT` holds and what the generated clients can decode, and what
//! makes the degenerate case at the top of the type unreachable instead of merely improbable.
//!
//! # A removal advances the epoch
//!
//! The epoch may stay put across versions — a role change needs no key rotation — but a roster
//! that omits a live member must carry a higher `amk_epoch` than the held one, or it is refused
//! as [`RosterOutcome::EpochNotAdvanced`]. Removal is an MLS `Remove`, which rotates the group's
//! keys, and the epoch a membership was granted at is what a federation capability is bound to:
//! a removal at the held epoch would let a re-admission at that same epoch carry the grant the
//! removed member held. The rule needs the held member set, so it is decided in the critical
//! section with the version comparison, by the crate-private `precheck` in both adapters.

use std::fmt;

pub use capsule_core::crypto::membership::MemberRole;
use jiff::Timestamp;
use uuid::Uuid;

use crate::store::{AlbumId, StoreFuture, UserId};

pub mod conformance;
pub mod memory;
pub mod postgres;

pub use self::memory::InMemoryMembership;
pub use self::postgres::PostgresMembership;

/// The stable column token for a role, and its inverse.
///
/// Here rather than on the core type because the token is a **storage** contract of this crate:
/// a row written as `writer` has to read back as `Writer` across every deploy, whatever the wire
/// spelling does.
pub fn role_token(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Reader => "reader",
        MemberRole::Writer => "writer",
    }
}

/// The role a stored token names, or `None` for a token no version of this server wrote.
pub fn role_from_token(token: &str) -> Option<MemberRole> {
    match token {
        "reader" => Some(MemberRole::Reader),
        "writer" => Some(MemberRole::Writer),
        _ => None,
    }
}

/// How far above the held version a roster may declare itself, and still be applied.
///
/// `roster_version` is the client's counter, and without a ceiling it is also a **latch**: one
/// publish at `u64::MAX` can never be superseded, because nothing can be strictly greater than
/// it, and the album's membership is frozen for good. That is a wedge no recovery path in this
/// design undoes — the store's comparison is the only ordering there is.
///
/// So a version is accepted only in the window `held+1 ..= held+16` (`held` reads as `0` for an
/// album with no roster yet). Sixteen because the gap a *legitimate* client opens is the number
/// of membership changes it made while it could not reach the server — a roster is a full
/// document, so it publishes only its latest — and sixteen offline changes to one album's
/// membership is already far past what the design describes. A client that does exceed it is
/// not stuck: the refusal names the held version, and the roster it re-signs at `held+1` says
/// exactly the same thing, because absence at a higher version *is* removal.
pub const MAX_ROSTER_VERSION_STEP: u64 = 16;

/// The widest `roster_version` any adapter will accept.
///
/// Two independent reasons, and they agree on the same number.
///
/// The durable adapter stores the counter in a `BIGINT`, so anything above `i64::MAX` is a
/// version Postgres cannot hold — `counter_to_column` refuses it. Deciding that at the port
/// instead means both adapters answer the same typed refusal rather than one answering a
/// storage failure, which is the divergence the container suite already caught once.
///
/// And every integer this server puts in a problem body is lowered by spargen as `i64`
/// (it emits no `u64` anywhere, `format: uint64` notwithstanding), so a counter above
/// `i64::MAX` would be a number the generated client cannot decode — and a decode failure is
/// not a typed API error, so the `code` and the recovery hint would be lost. Bounding the
/// counter here makes "every version the server can hold or name is decodable" true by
/// construction rather than by argument.
///
/// Reaching it legitimately would take ~9.2 × 10^18 publishes for one album.
pub const MAX_ROSTER_VERSION: u64 = i64::MAX as u64;

/// The roster the server currently holds for an album.
#[derive(Clone, PartialEq, Eq)]
pub struct RosterRecord {
    /// The album.
    pub album_id: AlbumId,
    /// Strictly monotonic per album; the idempotency key with `album_id`.
    pub roster_version: u64,
    /// The AMK epoch the roster reflects. Non-decreasing across versions.
    pub amk_epoch: u64,
    /// The owner-account device that signed it.
    pub attested_by_device: Uuid,
    /// When the server accepted it, on the server's clock.
    pub received_at: Timestamp,
    /// The signed document, verbatim canonical CBOR. Kept so a replay is decided on bytes and so
    /// an operator can re-verify what was accepted.
    pub document: Vec<u8>,
}

impl fmt::Debug for RosterRecord {
    /// The document is a few kilobytes of CBOR; a log line wants its length, not its bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RosterRecord")
            .field("album_id", &self.album_id)
            .field("roster_version", &self.roster_version)
            .field("amk_epoch", &self.amk_epoch)
            .field("attested_by_device", &self.attested_by_device)
            .field("received_at", &self.received_at)
            .field("document_len", &self.document.len())
            .finish()
    }
}

/// The version and epoch at which a member vanished from the roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revocation {
    /// The first roster version that omitted them.
    pub at_version: u64,
    /// The AMK epoch that roster carried — the epoch the owner bumped to on removal.
    pub at_epoch: u64,
}

/// What the server knows about one account's relationship to one album.
///
/// The owner is never a member here: the owner's access is the album record's own fact, and a
/// caller that needs both asks both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// Listed on the current roster.
    Member {
        /// What they may do.
        role: MemberRole,
        /// The epoch at which this continuous membership began. A re-admitted member gets the
        /// epoch of the roster that re-admitted them, not their original one.
        granted_epoch: u64,
    },
    /// Once listed, since omitted. The `403` case.
    Revoked(Revocation),
    /// Never listed. Indistinguishable from a stranger, by design.
    Never,
}

/// What applying a roster did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterOutcome {
    /// A newer roster replaced the held one (or there was none).
    Applied(RosterRecord),
    /// The same version with the same bytes: nothing changed, and the held record is returned.
    Replayed(RosterRecord),
    /// A version at or below the held one with different bytes. The client is behind.
    Stale {
        /// The version the server holds.
        current_version: u64,
    },
    /// A version more than [`MAX_ROSTER_VERSION_STEP`] above the held one. Not a roster that is
    /// behind — one so far ahead that accepting it would put the counter out of reach of every
    /// later publish.
    VersionLeap {
        /// The version the server holds (`0` when it holds no roster).
        current_version: u64,
        /// The highest version it would have accepted.
        max_version: u64,
    },
    /// A newer version that carried a lower AMK epoch than the held one. An epoch never goes
    /// backwards, so this is a client that lost state, not a legitimate roster.
    EpochRegressed {
        /// The version the server holds — the same re-sync hint `Stale` carries, so a route
        /// need not read the roster a second time to name it.
        current_version: u64,
        /// The epoch the server holds.
        stored: u64,
    },
    /// A newer version that omits a live member and carries the **same** AMK epoch as the held
    /// roster. A removal is an MLS `Remove`, and a `Remove` is a key rotation: a roster that
    /// drops someone without advancing the epoch describes a membership change the group's keys
    /// never made, and the removed member's grant would carry the epoch a later re-admission
    /// gets, so the binding a capability's `granted_epoch` relies on could not tell them apart.
    EpochNotAdvanced {
        /// The version the server holds.
        current_version: u64,
        /// The epoch the server holds, which the removing roster must exceed.
        stored: u64,
        /// How many live members the roster omits.
        removed: u64,
    },
}

/// Where membership is kept.
pub trait MembershipStore: fmt::Debug + Send + Sync {
    /// Replace the album's roster with `roster` naming `members`, in one critical section.
    ///
    /// The version comparison and the replacement are one operation. On `Applied`: every live
    /// member absent from `members` is marked revoked at the roster's version and epoch; every
    /// listed member is upserted, keeping their `granted_epoch` if they were already live and
    /// taking the roster's epoch if they are new or re-admitted. A user listed twice is taken
    /// once, last entry winning; the route refuses such a document before it reaches here.
    ///
    /// A roster that omits a live member must carry an `amk_epoch` above the held one; one that
    /// does not is [`RosterOutcome::EpochNotAdvanced`], decided inside the same critical section
    /// as the version comparison, because the held member set it is judged against is only
    /// stable there.
    ///
    /// `Stale`, `Replayed`, `VersionLeap`, `EpochRegressed` and `EpochNotAdvanced` change
    /// nothing.
    fn apply_roster(
        &self,
        roster: RosterRecord,
        members: Vec<(UserId, MemberRole)>,
    ) -> StoreFuture<'_, RosterOutcome>;

    /// What `user` is to `album`.
    fn membership<'a>(
        &'a self,
        album: &'a AlbumId,
        user: &'a UserId,
    ) -> StoreFuture<'a, Membership>;

    /// The roster the server holds for `album`, if any.
    fn current_roster<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<RosterRecord>>;
}

/// The membership module's collaborators.
#[derive(Debug, Clone)]
pub struct MembershipContext {
    members: std::sync::Arc<dyn MembershipStore>,
    clock: std::sync::Arc<dyn crate::store::Clock>,
}

impl MembershipContext {
    /// Assembles the module from its collaborators.
    pub fn new(
        members: std::sync::Arc<dyn MembershipStore>,
        clock: std::sync::Arc<dyn crate::store::Clock>,
    ) -> Self {
        Self { members, clock }
    }

    /// The store.
    pub fn members(&self) -> &dyn MembershipStore {
        self.members.as_ref()
    }

    /// The clock a roster's `received_at` is stamped from.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }
}

/// Decide what `incoming`, naming `members`, does to `held`, whose live members are `live`,
/// before any row is touched.
///
/// Pure, so both adapters make the same decision and the rule is testable without a store. `None`
/// is "apply it"; `Some` is the outcome that ends the operation without a write. Each adapter
/// reads `held` and `live` inside its critical section, so the member set a removal is judged
/// against is the one the write would replace.
pub(crate) fn precheck(
    held: Option<&RosterRecord>,
    live: &[UserId],
    incoming: &RosterRecord,
    members: &[(UserId, MemberRole)],
) -> Option<RosterOutcome> {
    // The ceiling first, and against a held version of `0` when there is no roster yet: a first
    // publish at `u64::MAX` would wedge the album exactly as a later one would, and an album
    // whose membership no publish can ever change is the one outcome this port must not be able
    // to reach. `saturating_add` so the window itself cannot overflow into wrapping around.
    let current_version = held.map_or(0, |held| held.roster_version);
    // Clamped to the ceiling as well as to the step: `saturating_add` alone would let
    // `max_version` sit above what an adapter can store and a client can decode, and — at the
    // very top of the type — collapse to `max_version == current_version`, where every later
    // publish is stale and the album is wedged after all. The clamp makes that unreachable
    // rather than merely improbable: a version above the ceiling is never accepted, so a held
    // version above it never exists.
    let max_version = current_version
        .saturating_add(MAX_ROSTER_VERSION_STEP)
        .min(MAX_ROSTER_VERSION);
    if incoming.roster_version > max_version {
        return Some(RosterOutcome::VersionLeap {
            current_version,
            max_version,
        });
    }
    let held = held?;
    if incoming.roster_version == held.roster_version {
        return Some(if incoming.document == held.document {
            RosterOutcome::Replayed(held.clone())
        } else {
            RosterOutcome::Stale {
                current_version: held.roster_version,
            }
        });
    }
    if incoming.roster_version < held.roster_version {
        return Some(RosterOutcome::Stale {
            current_version: held.roster_version,
        });
    }
    if incoming.amk_epoch < held.amk_epoch {
        return Some(RosterOutcome::EpochRegressed {
            current_version: held.roster_version,
            stored: held.amk_epoch,
        });
    }
    // A removal without a key rotation. Only an equal epoch reaches here — a lower one is the
    // regression above — and an equal epoch is legitimate for every change but a removal: a role
    // change or an addition recorded without a commit loses nothing, while a removal the epoch
    // does not follow leaves the removed member's grant indistinguishable from the one a later
    // re-admission at this epoch would get.
    if incoming.amk_epoch == held.amk_epoch {
        let listed: std::collections::BTreeSet<&UserId> =
            members.iter().map(|(user, _)| user).collect();
        let removed = live.iter().filter(|user| !listed.contains(user)).count();
        if removed > 0 {
            return Some(RosterOutcome::EpochNotAdvanced {
                current_version: held.roster_version,
                stored: held.amk_epoch,
                removed: u64::try_from(removed).unwrap_or(u64::MAX),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests;
