//! [`InMemoryMembership`] — the deterministic double.
//!
//! One mutex over both maps, which is what makes [`MembershipStore::apply_roster`] one critical
//! section: the version comparison and the replacement happen under the same lock.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::{
    MemberRole, Membership, MembershipStore, Revocation, RosterOutcome, RosterRecord, precheck,
};
use crate::store::{AlbumId, StoreFuture, UserId};

/// The deterministic membership store.
#[derive(Debug, Default)]
pub struct InMemoryMembership {
    inner: Mutex<Inner>,
}

/// One account's row for one album.
#[derive(Debug, Clone)]
struct MemberRow {
    role: MemberRole,
    granted_epoch: u64,
    revoked: Option<Revocation>,
}

#[derive(Debug, Default)]
struct Inner {
    rosters: BTreeMap<AlbumId, RosterRecord>,
    members: BTreeMap<(AlbumId, UserId), MemberRow>,
}

impl InMemoryMembership {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl MembershipStore for InMemoryMembership {
    fn apply_roster(
        &self,
        roster: RosterRecord,
        members: Vec<(UserId, MemberRole)>,
    ) -> StoreFuture<'_, RosterOutcome> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            if let Some(outcome) = precheck(inner.rosters.get(&roster.album_id), &roster) {
                return Ok(outcome);
            }
            let album = roster.album_id.clone();
            let listed: BTreeMap<UserId, MemberRole> = members.into_iter().collect();

            // Everyone live who is not on the new list vanished at this version and epoch.
            for ((row_album, user), row) in &mut inner.members {
                if row_album == &album && row.revoked.is_none() && !listed.contains_key(user) {
                    row.revoked = Some(Revocation {
                        at_version: roster.roster_version,
                        at_epoch: roster.amk_epoch,
                    });
                }
            }
            for (user, role) in listed {
                let key = (album.clone(), user);
                match inner.members.get_mut(&key) {
                    // Continuing: the role may change, the grant does not.
                    Some(row) if row.revoked.is_none() => row.role = role,
                    // New, or re-admitted: a fresh grant at this roster's epoch.
                    _ => {
                        inner.members.insert(
                            key,
                            MemberRow {
                                role,
                                granted_epoch: roster.amk_epoch,
                                revoked: None,
                            },
                        );
                    }
                }
            }
            tracing::info!(
                %album,
                roster_version = roster.roster_version,
                amk_epoch = roster.amk_epoch,
                "an album roster was applied"
            );
            inner.rosters.insert(album, roster.clone());
            Ok(RosterOutcome::Applied(roster))
        })
    }

    fn membership<'a>(
        &'a self,
        album: &'a AlbumId,
        user: &'a UserId,
    ) -> StoreFuture<'a, Membership> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            Ok(match inner.members.get(&(album.clone(), user.clone())) {
                None => Membership::Never,
                Some(row) => match row.revoked {
                    Some(revocation) => Membership::Revoked(revocation),
                    None => Membership::Member {
                        role: row.role,
                        granted_epoch: row.granted_epoch,
                    },
                },
            })
        })
    }

    fn current_roster<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<RosterRecord>> {
        Box::pin(async move { Ok(lock(&self.inner).rosters.get(album).cloned()) })
    }
}
