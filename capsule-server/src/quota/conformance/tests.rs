use std::sync::Arc;

use super::{Harness, run_all};
use crate::blob::ContentAddress;
use crate::quota::{InMemoryPulledHoldings, InMemoryQuota, QuotaStore};
use crate::store::{StoreFuture, UserId};

/// The deterministic ledger, counting the pulled holdings beside it.
#[derive(Debug)]
struct MemoryHarness {
    quotas: InMemoryQuota,
    pulled: Arc<InMemoryPulledHoldings>,
}

impl Default for MemoryHarness {
    fn default() -> Self {
        let pulled = Arc::new(InMemoryPulledHoldings::new());
        Self {
            quotas: InMemoryQuota::new().with_pulled(pulled.clone()),
            pulled,
        }
    }
}

impl Harness for MemoryHarness {
    fn quotas(&self) -> &dyn QuotaStore {
        &self.quotas
    }

    fn hold_pulled<'a>(
        &'a self,
        user: &'a UserId,
        album: &'a str,
        address: &'a ContentAddress,
        size: u64,
    ) -> StoreFuture<'a, ()> {
        self.pulled.hold(user, album, address, size);
        Box::pin(async { Ok(()) })
    }

    fn unsubscribe<'a>(&'a self, user: &'a UserId, album: &'a str) -> StoreFuture<'a, ()> {
        self.pulled.unsubscribe(user, album);
        Box::pin(async { Ok(()) })
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
    a_reservation_lapses_only_past_its_deadline,
    a_settled_reservation_is_never_released_as_expired,
    an_extension_moves_the_deadline_and_never_shortens_it,
    expired_reservations_are_listed_earliest_first_and_bounded,
    a_pulled_blob_counts_in_full_against_each_subscriber,
    a_pulled_blob_counts_once_across_a_subscribers_albums,
    a_pulled_blob_the_subscriber_uploaded_counts_once,
    a_pulled_blob_another_account_uploaded_still_counts_for_the_receiver,
    pulled_bytes_count_toward_the_hard_limit,
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
