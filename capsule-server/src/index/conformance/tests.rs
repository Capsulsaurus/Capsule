use super::*;
use crate::index::memory::InMemoryAssetIndex;

/// The deterministic double is only a legitimate stand-in for Postgres to the extent it
/// passes this.
#[tokio::test]
async fn the_in_memory_index_conforms() {
    run_all(&InMemoryAssetIndex::new()).await;
}

/// The double's custody receipts land in the log it was built over.
#[tokio::test]
async fn the_in_memory_index_writes_custody_receipts_with_its_rows() {
    let index = InMemoryAssetIndex::new();
    run_custody(&index, index.receipts().as_ref(), &suite_signer()).await;
}
