//! The one suite every [`MembershipStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **One critical section.** A roster at the held version with different bytes is `Stale`,
//!   not applied; a single-process suite cannot exhibit the race itself, so it asserts the
//!   consequence — the loser of a version tie changes nothing — and the structural guarantee
//!   stays in the adapter (one mutex here, one transaction lock in Postgres).
//! - **Removal is a stored fact.** A member omitted from a later roster answers
//!   [`Membership::Revoked`] with the version and epoch at which they vanished, never
//!   [`Membership::Never`]. That is what the blob route's `403` is rendered from.
//! - **A refusal changes nothing.** `Stale` and `EpochRegressed` leave both the roster and every
//!   member row exactly as they were.
//!
//! # Reusing a harness
//!
//! Every case scopes its own album ids, so cases may share one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

use super::{MemberRole, Membership, MembershipStore, Revocation, RosterOutcome, RosterRecord};
use crate::store::{AlbumId, StoreError, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The membership store under test.
    fn members(&self) -> &dyn MembershipStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming membership store must succeed at {doing}: {error}"),
    }
}

/// `case`'s own album.
fn album(case: &str) -> AlbumId {
    AlbumId::new(format!("{case}-album"))
}

/// `case`'s account `name`.
fn user(case: &str, name: &str) -> UserId {
    UserId::new(format!("{case}-{name}"))
}

/// A roster record for `case` at `version` and `epoch`, whose bytes differ per version and per
/// `variant` so a "same version, different bytes" case can be written.
fn roster(case: &str, version: u64, epoch: u64, variant: &str) -> RosterRecord {
    RosterRecord {
        album_id: album(case),
        roster_version: version,
        amk_epoch: epoch,
        attested_by_device: Uuid::from_u128(0xD1),
        received_at: Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000),
        document: format!("{case}/v{version}/e{epoch}/{variant}").into_bytes(),
    }
}

/// Apply `roster` naming `members`, expecting it to be accepted.
async fn apply(
    h: &dyn Harness,
    roster: RosterRecord,
    members: Vec<(UserId, MemberRole)>,
) -> RosterOutcome {
    ok(
        h.members().apply_roster(roster, members).await,
        "apply a roster",
    )
}

/// What the store says `user` is to `case`'s album.
async fn membership(h: &dyn Harness, case: &str, user: &UserId) -> Membership {
    ok(
        h.members().membership(&album(case), user).await,
        "read a membership",
    )
}

// ===========================================================================================
// Applying rosters
// ===========================================================================================

/// The first roster is applied and its members are members, with the roster's epoch.
pub async fn the_first_roster_is_applied_and_its_members_are_members(h: &dyn Harness) {
    let case = "first";
    let bob = user(case, "bob");
    let outcome = apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;
    let RosterOutcome::Applied(record) = outcome else {
        panic!("the first roster must be applied, got {outcome:?}");
    };
    assert_eq!(record.roster_version, 1);
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Writer,
            granted_epoch: 1,
        }
    );
    let held = ok(
        h.members().current_roster(&album(case)).await,
        "read the current roster",
    )
    .expect("a roster is held");
    assert_eq!(
        held, record,
        "current_roster returns what apply_roster returned"
    );
}

/// The held record is the stored record, instant included.
///
/// An adapter that keeps sub-microsecond precision in Rust and drops it in the column would hand
/// `apply_roster`'s caller a record the next `current_roster` does not produce.
pub async fn the_returned_record_is_the_record_the_next_read_produces(h: &dyn Harness) {
    let case = "precise";
    let mut precise = roster(case, 1, 1, "");
    precise.received_at =
        Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    let RosterOutcome::Applied(returned) = apply(h, precise, vec![]).await else {
        panic!("applied");
    };
    let held = ok(
        h.members().current_roster(&album(case)).await,
        "read the current roster",
    )
    .expect("a roster is held");
    assert_eq!(held.received_at, returned.received_at);
    assert_eq!(held.document, returned.document);
}

/// The same bytes again are a replay: the held record comes back and nothing changes.
pub async fn the_same_bytes_again_are_a_replay(h: &dyn Harness) {
    let case = "replay";
    let bob = user(case, "bob");
    let first = roster(case, 1, 1, "");
    let RosterOutcome::Applied(record) =
        apply(h, first.clone(), vec![(bob.clone(), MemberRole::Writer)]).await
    else {
        panic!("applied");
    };
    // The member list is deliberately *different* on the replay: a replay is decided on the
    // document's bytes, which the route derived the list from, not on the list itself.
    assert_eq!(
        apply(h, first, vec![]).await,
        RosterOutcome::Replayed(record),
        "identical bytes at the held version are a replay"
    );
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Writer,
            granted_epoch: 1,
        },
        "a replay changes nothing"
    );
}

/// A version at or below the held one with different bytes is stale, and changes nothing.
pub async fn a_stale_version_is_refused_and_changes_nothing(h: &dyn Harness) {
    let case = "stale";
    let bob = user(case, "bob");
    let carol = user(case, "carol");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;

    // Same version, different bytes: the loser of a concurrent publish.
    assert_eq!(
        apply(
            h,
            roster(case, 1, 1, "other"),
            vec![(carol.clone(), MemberRole::Writer)]
        )
        .await,
        RosterOutcome::Stale { current_version: 1 }
    );
    // A lower version: a client that is behind.
    assert_eq!(
        apply(
            h,
            roster(case, 0, 1, ""),
            vec![(carol.clone(), MemberRole::Writer)]
        )
        .await,
        RosterOutcome::Stale { current_version: 1 }
    );
    assert_eq!(membership(h, case, &carol).await, Membership::Never);
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Writer,
            granted_epoch: 1,
        }
    );
    assert_eq!(
        ok(
            h.members().current_roster(&album(case)).await,
            "read the current roster"
        )
        .expect("held")
        .roster_version,
        1
    );
    // And the album is not left locked by the refusals: the next real version applies. An
    // adapter whose early return leaked its critical section would hang here, visibly.
    assert!(matches!(
        apply(h, roster(case, 2, 1, ""), vec![(bob, MemberRole::Writer)]).await,
        RosterOutcome::Applied(_)
    ));
}

/// A newer version carrying a lower epoch is a regression, and changes nothing.
pub async fn an_epoch_regression_is_refused_and_changes_nothing(h: &dyn Harness) {
    let case = "regress";
    let bob = user(case, "bob");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;
    assert_eq!(
        apply(h, roster(case, 2, 0, ""), vec![]).await,
        RosterOutcome::EpochRegressed { stored: 1 }
    );
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Writer,
            granted_epoch: 1,
        },
        "the member was not revoked by a refused roster"
    );
    assert_eq!(
        ok(
            h.members().current_roster(&album(case)).await,
            "read the current roster"
        )
        .expect("held")
        .roster_version,
        1
    );
    assert!(
        matches!(
            apply(h, roster(case, 2, 1, ""), vec![(bob, MemberRole::Writer)]).await,
            RosterOutcome::Applied(_)
        ),
        "the refusal released the album for the next version"
    );
}

/// One application revokes, continues and admits at once.
///
/// The case that runs the adapters' set arithmetic with more than one name on each side: the
/// omitted member is revoked, the continuing one keeps their grant, the new one gets a fresh
/// grant — in one operation, from one list.
pub async fn one_application_revokes_continues_and_admits(h: &dyn Harness) {
    let case = "mixed";
    let bob = user(case, "bob");
    let carol = user(case, "carol");
    let dave = user(case, "dave");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![
            (bob.clone(), MemberRole::Writer),
            (carol.clone(), MemberRole::Reader),
        ],
    )
    .await;
    apply(
        h,
        roster(case, 2, 2, ""),
        vec![
            (carol.clone(), MemberRole::Writer),
            (dave.clone(), MemberRole::Reader),
        ],
    )
    .await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Revoked(Revocation {
            at_version: 2,
            at_epoch: 2,
        })
    );
    assert_eq!(
        membership(h, case, &carol).await,
        Membership::Member {
            role: MemberRole::Writer,
            granted_epoch: 1,
        }
    );
    assert_eq!(
        membership(h, case, &dave).await,
        Membership::Member {
            role: MemberRole::Reader,
            granted_epoch: 2,
        }
    );
}

/// An account listed twice is taken once, the last entry winning.
///
/// The route refuses such a document, so this is the port's promise rather than a wire case —
/// and it is asserted because an adapter that folded the list into one multi-row statement would
/// fail it loudly rather than diverge quietly.
pub async fn an_account_listed_twice_is_taken_once_last_entry_winning(h: &dyn Harness) {
    let case = "twice";
    let bob = user(case, "bob");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![
            (bob.clone(), MemberRole::Writer),
            (bob.clone(), MemberRole::Reader),
        ],
    )
    .await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Reader,
            granted_epoch: 1,
        }
    );
}

// ===========================================================================================
// Membership over time
// ===========================================================================================

/// A member omitted from a later roster is revoked at that roster's version and epoch.
///
/// The `403` case: the row is retained and marked, never deleted, or a former member would be
/// indistinguishable from a stranger.
pub async fn an_omitted_member_is_revoked_at_the_rosters_version_and_epoch(h: &dyn Harness) {
    let case = "revoke";
    let bob = user(case, "bob");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;
    apply(h, roster(case, 2, 2, ""), vec![]).await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Revoked(Revocation {
            at_version: 2,
            at_epoch: 2,
        })
    );
    // And a *further* roster that still omits them does not move the revocation.
    apply(h, roster(case, 3, 3, ""), vec![]).await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Revoked(Revocation {
            at_version: 2,
            at_epoch: 2,
        }),
        "the revocation records the first omission, not the latest roster"
    );
}

/// A re-admitted member is a member again, with a fresh grant at the re-admitting epoch.
pub async fn a_re_admitted_member_gets_a_fresh_grant(h: &dyn Harness) {
    let case = "readmit";
    let bob = user(case, "bob");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;
    apply(h, roster(case, 2, 2, ""), vec![]).await;
    apply(
        h,
        roster(case, 3, 3, ""),
        vec![(bob.clone(), MemberRole::Reader)],
    )
    .await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Reader,
            granted_epoch: 3,
        }
    );
}

/// A continuing member's role follows the roster and their grant does not move.
pub async fn a_continuing_members_role_changes_and_their_grant_does_not(h: &dyn Harness) {
    let case = "continue";
    let bob = user(case, "bob");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;
    apply(
        h,
        roster(case, 2, 2, ""),
        vec![(bob.clone(), MemberRole::Reader)],
    )
    .await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Member {
            role: MemberRole::Reader,
            granted_epoch: 1,
        },
        "the grant is the epoch this continuous membership began at"
    );
}

/// An account never listed is `Never`, and so is any account on an album with no roster.
pub async fn an_unlisted_account_is_never_a_member(h: &dyn Harness) {
    let case = "never";
    let bob = user(case, "bob");
    let carol = user(case, "carol");
    assert_eq!(membership(h, case, &carol).await, Membership::Never);
    assert_eq!(
        ok(
            h.members().current_roster(&album(case)).await,
            "read the current roster"
        ),
        None
    );
    apply(h, roster(case, 1, 1, ""), vec![(bob, MemberRole::Reader)]).await;
    assert_eq!(membership(h, case, &carol).await, Membership::Never);
}

/// Membership is per album: the same account on two albums has two independent answers.
pub async fn membership_does_not_leak_between_albums(h: &dyn Harness) {
    let case = "isolated";
    let other = "isolated-other";
    let bob = user(case, "bob");
    apply(
        h,
        roster(case, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Writer)],
    )
    .await;
    apply(
        h,
        roster(other, 1, 1, ""),
        vec![(bob.clone(), MemberRole::Reader)],
    )
    .await;
    // Revoking on one album leaves the other untouched.
    apply(h, roster(case, 2, 2, ""), vec![]).await;
    assert_eq!(
        membership(h, case, &bob).await,
        Membership::Revoked(Revocation {
            at_version: 2,
            at_epoch: 2,
        })
    );
    assert_eq!(
        membership(h, other, &bob).await,
        Membership::Member {
            role: MemberRole::Reader,
            granted_epoch: 1,
        }
    );
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    the_first_roster_is_applied_and_its_members_are_members(h).await;
    the_returned_record_is_the_record_the_next_read_produces(h).await;
    the_same_bytes_again_are_a_replay(h).await;
    a_stale_version_is_refused_and_changes_nothing(h).await;
    an_epoch_regression_is_refused_and_changes_nothing(h).await;
    one_application_revokes_continues_and_admits(h).await;
    an_account_listed_twice_is_taken_once_last_entry_winning(h).await;

    an_omitted_member_is_revoked_at_the_rosters_version_and_epoch(h).await;
    a_re_admitted_member_gets_a_fresh_grant(h).await;
    a_continuing_members_role_changes_and_their_grant_does_not(h).await;
    an_unlisted_account_is_never_a_member(h).await;
    membership_does_not_leak_between_albums(h).await;
}

#[cfg(test)]
mod tests {
    use super::{Harness, run_all};
    use crate::membership::{InMemoryMembership, MembershipStore};

    /// The deterministic store.
    #[derive(Debug, Default)]
    struct MemoryHarness {
        members: InMemoryMembership,
    }

    impl Harness for MemoryHarness {
        fn members(&self) -> &dyn MembershipStore {
            &self.members
        }
    }

    /// Declares one `#[tokio::test]` per conformance case, on a fresh store each.
    macro_rules! conformance_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[tokio::test]
                async fn $case() {
                    super::$case(&MemoryHarness::default()).await;
                }
            )+
        };
    }

    conformance_cases! {
        the_first_roster_is_applied_and_its_members_are_members,
        the_returned_record_is_the_record_the_next_read_produces,
        the_same_bytes_again_are_a_replay,
        a_stale_version_is_refused_and_changes_nothing,
        an_epoch_regression_is_refused_and_changes_nothing,
        one_application_revokes_continues_and_admits,
        an_account_listed_twice_is_taken_once_last_entry_winning,
        an_omitted_member_is_revoked_at_the_rosters_version_and_epoch,
        a_re_admitted_member_gets_a_fresh_grant,
        a_continuing_members_role_changes_and_their_grant_does_not,
        an_unlisted_account_is_never_a_member,
        membership_does_not_leak_between_albums,
    }

    /// The whole suite, in one pass on one store — the entry point the container case uses.
    #[tokio::test]
    async fn the_in_memory_store_conforms() {
        run_all(&MemoryHarness::default()).await;
    }
}
