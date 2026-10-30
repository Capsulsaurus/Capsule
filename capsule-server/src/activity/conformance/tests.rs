use super::{Harness, run_all};
use crate::activity::{ActivityStore, InMemoryActivity};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    activity: InMemoryActivity,
}

impl Harness for MemoryHarness {
    fn activity(&self) -> &dyn ActivityStore {
        &self.activity
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
    an_untouched_account_has_an_empty_log,
    an_event_reads_back_exactly_as_recorded,
    every_kind_and_absent_field_round_trips,
    instants_read_back_to_the_microsecond,
    the_log_is_newest_first_in_recording_order,
    a_log_is_scoped_to_its_account,
    a_read_returns_the_newest_limit_events,
    a_read_excludes_events_before_its_since,
    an_append_deletes_its_accounts_expired_events,
    an_append_keeps_at_most_the_bound,
    a_duplicate_event_id_is_refused_and_the_first_stands,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
