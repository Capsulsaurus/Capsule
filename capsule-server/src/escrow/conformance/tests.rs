use super::{Harness, run_all};
use crate::escrow::{EscrowStore, InMemoryEscrow};

/// The deterministic store.
#[derive(Debug, Default)]
struct MemoryHarness {
    escrows: InMemoryEscrow,
}

impl Harness for MemoryHarness {
    fn escrows(&self) -> &dyn EscrowStore {
        &self.escrows
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
    an_account_with_no_escrow_fetches_none,
    the_first_escrow_is_not_a_replacement_and_reads_back,
    a_second_escrow_replaces_the_first_and_the_old_one_is_gone,
    every_store_after_the_first_is_a_replacement,
    the_blob_is_returned_byte_for_byte,
    escrows_do_not_leak_between_accounts,
    the_stored_instant_reads_back_at_microsecond_precision,
    an_account_with_no_sealed_identity_fetches_none,
    a_sealed_identity_replaces_and_reads_back_byte_for_byte,
    sealed_identities_do_not_leak_between_accounts,
    the_escrow_and_the_sealed_identity_are_independent,
    the_sealed_identity_instant_reads_back_at_microsecond_precision,
    an_account_with_no_album_keys_lists_none,
    album_keys_replace_per_album_and_list_in_album_order,
    album_keys_do_not_leak_and_leave_the_other_slots_alone,
    the_album_keys_instant_reads_back_at_microsecond_precision,
    album_keys_are_bounded_per_account,
}

/// The whole suite, in one pass on one store — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_store_conforms() {
    run_all(&MemoryHarness::default()).await;
}
