use super::{Harness, run_all};
use crate::gc::CollectionStore;
use crate::gc::memory::InMemoryCollection;

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    marks: InMemoryCollection,
}

impl Harness for MemoryHarness {
    fn marks(&self) -> &dyn CollectionStore {
        &self.marks
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
    an_unmarked_address_has_no_mark,
    the_first_mark_is_recorded_with_its_instant,
    marking_twice_is_one_mark_restamped,
    the_mark_instant_round_trips_to_the_microsecond,
    unmarking_removes_the_mark_once,
    unmarking_an_unknown_address_changes_nothing,
    a_mark_after_an_unmark_is_a_fresh_mark,
    marks_do_not_leak_between_addresses,
    the_listing_is_in_address_order,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}

/// #672 over the in-memory index and mark store. The Postgres pair runs in
/// `index::postgres`'s container case.
#[tokio::test]
async fn a_shared_blob_survives_purging_one_holder() {
    super::a_shared_blob_survives_purging_one_holder(
        std::sync::Arc::new(crate::index::memory::InMemoryAssetIndex::new()),
        std::sync::Arc::new(InMemoryCollection::default()),
    )
    .await;
}
