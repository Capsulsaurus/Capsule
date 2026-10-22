//! The anchoring decorator: every append records its head; reads go straight through.
//!
//! That an index failure does not fail the append is `tests/upload.rs`'s, over the switchable
//! index double the integration fixture already carries.

use capsule_core::crypto::keys::HybridSigningKey;

use super::*;
use crate::attestation::LocalAttestationKey;
use crate::index::memory::InMemoryAssetIndex;
use crate::ledger::InMemoryLedger;

fn signer() -> LocalAttestationKey {
    LocalAttestationKey::new("server.test", HybridSigningKey::from_seed64(&[7; 64]))
}

fn directory(version: u64) -> LedgerEvent {
    LedgerEvent::Directory {
        user_id: "user-1".to_owned(),
        directory_version: version,
        document_hash: capsule_core::crypto::hash::hash_bytes(&version.to_be_bytes()),
    }
}

#[tokio::test]
async fn each_append_records_its_head_in_the_index() {
    let index = Arc::new(InMemoryAssetIndex::new());
    let inner = Arc::new(InMemoryLedger::new());
    let ledger = AnchoredLedger::new(inner.clone(), index.clone());
    let owner = OwnerId::new("owner-1");
    let signer = signer();

    assert_eq!(index.ledger_head(&owner).await.expect("read"), None);
    for version in 1..=3 {
        let entry = ledger
            .append(&owner, directory(version), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("an append lands");
        assert_eq!(
            index.ledger_head(&owner).await.expect("read"),
            Some(LedgerHead {
                seq: entry.core.seq,
                hash: entry.chain_hash(),
            }),
            "the anchor is not the entry just appended"
        );
    }

    assert_eq!(ledger.owners().await.expect("owners"), vec![owner.clone()]);
    assert_eq!(
        ledger.scan(&owner).await.expect("scan"),
        inner.scan(&owner).await.expect("scan")
    );
}

/// Each owner's appends move only that owner's anchor.
#[tokio::test]
async fn one_owners_append_leaves_anothers_anchor_alone() {
    let index = Arc::new(InMemoryAssetIndex::new());
    let ledger = AnchoredLedger::new(Arc::new(InMemoryLedger::new()), index.clone());
    let alice = OwnerId::new("alice");
    let bob = OwnerId::new("bob");
    let signer = signer();

    ledger
        .append(&alice, directory(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("an append lands");
    assert_eq!(index.ledger_head(&bob).await.expect("read"), None);
    assert_eq!(
        index
            .ledger_head(&alice)
            .await
            .expect("read")
            .map(|head| head.seq),
        Some(1)
    );
}
