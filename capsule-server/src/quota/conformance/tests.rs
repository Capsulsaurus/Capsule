use super::{Harness, run_all};
use crate::quota::{InMemoryQuota, QuotaStore};

/// The deterministic ledger.
#[derive(Debug, Default)]
struct MemoryHarness {
    quotas: InMemoryQuota,
}

impl Harness for MemoryHarness {
    fn quotas(&self) -> &dyn QuotaStore {
        &self.quotas
    }
}

/// Declares one `#[tokio::test]` per conformance case.
///
/// One test each, on a fresh ledger each: a failure names the property that broke, and no
/// case can pass because a previous one left the ledger in a convenient state.
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
    charging_the_same_address_twice_debits_once,
    the_already_attributed_answer_says_nothing_about_who_holds_it,
    an_uncharged_account_owes_nothing,
    releasing_a_reservation_credits_the_bytes_back,
    another_accounts_reservation_is_not_releasable,
    the_collector_releases_by_address_and_the_ledger_names_the_account,
    a_contributors_attribution_survives_until_the_collector_releases_it,
    releasing_an_unattributed_address_is_none_rather_than_an_error,
    the_crossing_is_stamped_once_and_cleared_by_going_under,
    a_collector_release_clears_the_over_limit_clock,
    parallel_charges_cannot_overshoot_the_hard_limit,
    a_suspension_is_recorded_once_and_lifted,
    a_suspension_is_independent_of_the_usage,
}

/// The whole suite, in one pass on one ledger.
///
/// The entry point a container-backed adapter uses, so it is exercised here too — otherwise
/// the first time anyone ran it would be against Postgres, where a failure is hardest to
/// read. It also proves the cases really are independent.
#[tokio::test]
async fn the_in_memory_ledger_conforms() {
    run_all(&MemoryHarness::default()).await;
}
