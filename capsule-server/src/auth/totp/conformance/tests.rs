use super::{Harness, run_all};
use crate::auth::totp::{InMemoryTotp, TotpStore};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    enrollments: InMemoryTotp,
}

impl Harness for MemoryHarness {
    fn enrollments(&self) -> &dyn TotpStore {
        &self.enrollments
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
    a_first_begin_starts_a_pending_enrollment,
    a_pending_enrollment_is_replaced,
    an_active_enrollment_refuses_begin_and_changes_nothing,
    activation_promotes_and_spends_its_step,
    activation_is_pending_only,
    consumption_is_a_compare_and_set,
    consumption_without_an_enrollment_is_not_enrolled,
    racing_consumptions_admit_exactly_one,
    disabling_removes_the_enrollment_once,
    enrollments_do_not_leak_between_accounts,
    the_instants_round_trip_to_the_microsecond,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
