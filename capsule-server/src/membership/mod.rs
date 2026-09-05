//! Album membership (`S-C51`): the one fact the key-free server holds about who may read and
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
//! design/import/download-sync.md requires is gone.
//!
//! # One critical section
//!
//! [`MembershipStore::apply_roster`] compares versions and replaces the roster in **one**
//! operation, the way the device directory's `publish` does: two concurrent publishes cannot
//! both read "version 1 is current" and both write version 2. The in-memory adapter holds one
//! mutex; the Postgres adapter takes a per-album transaction lock.

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
    /// A newer version that carried a lower AMK epoch than the held one. An epoch never goes
    /// backwards, so this is a client that lost state, not a legitimate roster.
    EpochRegressed {
        /// The epoch the server holds.
        stored: u64,
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
    /// `Stale`, `Replayed` and `EpochRegressed` change nothing.
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

/// Decide what `incoming` does to `held`, before any row is touched.
///
/// Pure, so both adapters make the same decision and the rule is testable without a store. `None`
/// is "apply it"; `Some` is the outcome that ends the operation without a write.
pub(crate) fn precheck(
    held: Option<&RosterRecord>,
    incoming: &RosterRecord,
) -> Option<RosterOutcome> {
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
            stored: held.amk_epoch,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(version: u64, epoch: u64, document: &[u8]) -> RosterRecord {
        RosterRecord {
            album_id: AlbumId::new("album"),
            roster_version: version,
            amk_epoch: epoch,
            attested_by_device: Uuid::from_u128(1),
            received_at: Timestamp::UNIX_EPOCH,
            document: document.to_vec(),
        }
    }

    #[test]
    fn the_first_roster_is_always_applied() {
        assert_eq!(precheck(None, &record(1, 1, b"a")), None);
        // Even a version 0 or an epoch 0: monotonicity is against the *held* roster only.
        assert_eq!(precheck(None, &record(0, 0, b"a")), None);
    }

    #[test]
    fn the_same_version_is_a_replay_on_identical_bytes_and_stale_otherwise() {
        let held = record(1, 1, b"a");
        assert_eq!(
            precheck(Some(&held), &record(1, 1, b"a")),
            Some(RosterOutcome::Replayed(held.clone()))
        );
        assert_eq!(
            precheck(Some(&held), &record(1, 1, b"b")),
            Some(RosterOutcome::Stale { current_version: 1 })
        );
    }

    #[test]
    fn a_lower_version_is_stale_whatever_its_bytes_or_epoch() {
        let held = record(2, 2, b"a");
        assert_eq!(
            precheck(Some(&held), &record(1, 9, b"a")),
            Some(RosterOutcome::Stale { current_version: 2 })
        );
    }

    #[test]
    fn a_newer_version_with_a_lower_epoch_is_a_regression() {
        let held = record(1, 3, b"a");
        assert_eq!(
            precheck(Some(&held), &record(2, 2, b"b")),
            Some(RosterOutcome::EpochRegressed { stored: 3 })
        );
        // Equal is fine: a roster may change without a key rotation.
        assert_eq!(precheck(Some(&held), &record(2, 3, b"b")), None);
        assert_eq!(precheck(Some(&held), &record(2, 4, b"b")), None);
    }

    #[test]
    fn the_role_tokens_round_trip_and_nothing_else_parses() {
        for role in [MemberRole::Reader, MemberRole::Writer] {
            assert_eq!(role_from_token(role_token(role)), Some(role));
        }
        assert_eq!(role_from_token("admin"), None);
    }

    #[test]
    fn a_roster_records_debug_shows_the_documents_length_not_its_bytes() {
        let rendered = format!("{:?}", record(1, 1, b"secret-bytes"));
        assert!(rendered.contains("document_len: 12"), "{rendered}");
        assert!(!rendered.contains("secret"), "{rendered}");
    }
}
