use super::{Harness, run_all};
use crate::drop::{DropStore, InMemoryDrops};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    drops: InMemoryDrops,
}

impl Harness for MemoryHarness {
    fn drops(&self) -> &dyn DropStore {
        &self.drops
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
    an_unknown_link_resolves_to_none_and_is_not_live,
    a_provisioned_link_resolves_to_its_whole_record,
    provisioning_a_held_id_is_refused_and_the_link_is_unchanged,
    the_owner_cannot_reprovision_a_spent_or_revoked_link,
    an_admission_reserves_one_file_and_its_bytes,
    a_file_past_the_size_limit_is_refused_first_and_reserves_nothing,
    the_file_count_cap_is_never_exceeded,
    the_byte_cap_admits_an_exact_fit_and_nothing_more,
    a_single_use_link_is_spent_by_its_first_drop,
    an_expired_link_is_not_live,
    revocation_is_the_owners_once_and_ends_admission,
    a_refund_gives_back_a_reservation_and_floors_at_zero,
    a_declaration_is_taken_back_exactly_once,
    a_second_declaration_for_a_session_replaces_the_first,
    a_declaration_is_filed_into_the_inbox_exactly_once,
    filing_a_drop_already_in_the_inbox_keeps_the_row,
    the_inbox_is_the_owners_oldest_first,
    a_claim_holds_the_row_once,
    a_released_row_can_be_claimed_again,
    a_settled_row_leaves_the_inbox,
    a_discard_is_the_owners,
    redepositing_a_held_drop_id_replaces_the_entry,
    instants_read_back_at_microsecond_precision,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
