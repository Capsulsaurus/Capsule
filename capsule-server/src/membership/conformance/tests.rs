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
    a_version_leap_is_refused_and_the_album_still_takes_its_next_roster,
    the_version_window_binds_an_albums_first_roster,
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
