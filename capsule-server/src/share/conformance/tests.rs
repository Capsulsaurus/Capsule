use super::{Harness, run_all};
use crate::share::{InMemoryShares, ShareStore};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    shares: InMemoryShares,
}

impl Harness for MemoryHarness {
    fn shares(&self) -> &dyn ShareStore {
        &self.shares
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
    an_unknown_link_resolves_to_none,
    an_issued_link_resolves_to_its_whole_record,
    the_optional_fields_read_back_absent,
    a_link_serves_exactly_what_it_enumerates,
    the_wrapped_secret_is_returned_byte_for_byte,
    instants_read_back_at_microsecond_precision,
    issuing_a_held_id_is_refused_and_the_link_is_unchanged,
    the_owner_cannot_reissue_or_unrevoke_a_held_id,
    the_owner_revokes_and_the_link_still_resolves_revoked,
    a_second_revocation_changes_nothing,
    only_the_owner_can_revoke,
    revocation_does_not_leak_between_links,
    a_live_link_names_every_address_it_serves,
    a_revoked_or_expired_link_names_nothing,
    a_shared_address_is_held_while_any_naming_link_is_live,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
