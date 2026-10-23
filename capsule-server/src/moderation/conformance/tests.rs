use super::{Harness, run_all};
use crate::moderation::{InMemoryModeration, ModerationStore};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    moderation: InMemoryModeration,
}

impl Harness for MemoryHarness {
    fn moderation(&self) -> &dyn ModerationStore {
        &self.moderation
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
    an_untouched_account_is_active_with_an_empty_record,
    a_suspension_moves_the_standing_and_is_recorded,
    a_reinstatement_lifts_the_suspension_and_does_not_erase_it,
    an_asset_action_records_without_touching_standing,
    the_record_is_in_the_order_it_was_applied,
    a_record_is_scoped_to_its_account,
    every_action_and_absent_field_round_trips,
    instants_read_back_to_the_microsecond,
    a_filed_report_is_pending_exactly_as_filed,
    a_duplicate_report_id_is_refused_and_the_first_stands,
    the_queue_reads_in_report_id_order,
    filing_a_report_changes_no_standing_and_writes_no_event,
    a_reports_instants_read_back_to_the_microsecond,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
