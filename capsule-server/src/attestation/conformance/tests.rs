use super::{Harness, run_all};
use crate::attestation::{InMemoryReceipts, ReceiptLog};

/// The deterministic log.
#[derive(Debug, Default)]
struct MemoryHarness {
    receipts: InMemoryReceipts,
}

impl Harness for MemoryHarness {
    fn receipts(&self) -> &dyn ReceiptLog {
        &self.receipts
    }
}

/// Declares one `#[tokio::test]` per conformance case, on a fresh log each.
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
    the_first_receipt_opens_the_chain,
    the_chain_is_monotonic_and_each_entry_names_its_predecessor,
    concurrent_issues_do_not_fork_the_chain,
    a_reissued_upload_returns_the_same_receipt,
    the_receipt_carries_the_draft_and_verifies_under_its_signer,
    a_receipt_reads_back_byte_for_byte,
    receipts_are_found_by_upload_and_by_asset,
    an_unknown_upload_or_asset_has_no_receipt,
}

/// The whole suite, in one pass on one log — the entry point the container case uses.
#[tokio::test]
async fn the_in_memory_log_conforms() {
    run_all(&MemoryHarness::default()).await;
}
