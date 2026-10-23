use super::{Harness, run_all};
use crate::directory::{DeviceDirectoryStore, InMemoryDeviceDirectory};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    directories: InMemoryDeviceDirectory,
}

impl Harness for MemoryHarness {
    fn directories(&self) -> &dyn DeviceDirectoryStore {
        &self.directories
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
    a_first_publish_is_accepted_and_served_back_verbatim,
    an_account_that_never_published_fetches_none,
    an_advancing_version_replaces_the_directory,
    a_version_that_does_not_advance_is_stale_and_changes_nothing,
    a_different_key_is_an_identity_mismatch_and_changes_nothing,
    a_wrong_key_with_a_stale_version_is_refused_as_a_mismatch,
    the_anchor_is_the_first_publishes_key_for_good,
    a_directory_is_scoped_to_its_account,
    the_publication_instant_reads_back_to_the_microsecond,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
