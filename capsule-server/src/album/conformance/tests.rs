use super::{Harness, run_all};
use crate::album::{AlbumStore, InMemoryAlbums};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    albums: InMemoryAlbums,
}

impl Harness for MemoryHarness {
    fn albums(&self) -> &dyn AlbumStore {
        &self.albums
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
    a_first_provision_creates_the_album_the_next_read_returns,
    an_unprovisioned_album_reads_as_none,
    re_provisioning_by_the_owner_answers_the_stored_record_and_writes_nothing,
    an_id_bound_elsewhere_is_not_available_and_changes_nothing,
    the_returned_instants_are_the_instants_the_next_read_produces,
    a_proposal_quiesces_the_album,
    a_replayed_proposal_answers_quiescing_again,
    a_second_live_ceremony_conflicts_and_changes_nothing,
    an_expired_ceremony_is_replaced_by_the_next_proposal,
    a_stranger_or_an_unknown_album_is_not_found_and_nothing_is_written,
    a_stranger_cannot_end_the_owners_ceremony,
    ending_the_live_ceremony_clears_it,
    ending_when_nothing_is_in_flight_is_cleared,
    ending_a_different_live_ceremony_conflicts_and_changes_nothing,
    an_expired_ceremony_is_cleared_whatever_id_ends_it,
    a_ceremony_does_not_leak_between_albums,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
