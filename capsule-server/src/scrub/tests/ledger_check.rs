//! Check 6: every owner ledger verifies, and every write it records has its row.

use capsule_core::crypto::keys::HybridSigningKey;

use super::*;
use crate::attestation::{LocalAttestationKey, PublishedKey, ReceiptSigner as _};
use crate::ledger::replay::LedgerKeys;
use crate::ledger::{ChainFault, InMemoryLedger, LedgerEvent};

fn signer(seed: u8) -> LocalAttestationKey {
    LocalAttestationKey::new("server.test", HybridSigningKey::from_seed64(&[seed; 64]))
}

fn keys(signer: &LocalAttestationKey) -> LedgerKeys {
    LedgerKeys::new(vec![PublishedKey {
        key_id: signer.key_id(),
        public: signer.verifying_key(),
        active_from: Timestamp::UNIX_EPOCH,
        active_to: None,
    }])
}

fn custody(upload: &str, asset: &AssetId) -> LedgerEvent {
    LedgerEvent::Custody {
        upload_id: upload.to_owned(),
        asset_id: asset.as_str().to_owned(),
        blob_role: "metadata".to_owned(),
        content_hash: hash_bytes(upload.as_bytes()),
        size: 1,
        manifest_sha256: None,
        upload_user_id: "scrub-owner".to_owned(),
        uploader_device: None,
        received_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn indexed(upload: &str, asset: &AssetId) -> LedgerEvent {
    LedgerEvent::Indexed {
        upload_id: upload.to_owned(),
        asset_id: asset.as_str().to_owned(),
        sync_seq: Some(1),
        receipt: None,
    }
}

async fn append(ledger: &InMemoryLedger, signer: &LocalAttestationKey, event: LedgerEvent) {
    ledger
        .append(
            &OwnerId::new("scrub-owner"),
            event,
            Timestamp::UNIX_EPOCH,
            signer,
        )
        .await
        .expect("an append lands");
}

#[tokio::test]
async fn a_ledger_whose_writes_all_have_rows_is_clean() {
    let h = Harness::new();
    h.publish("ledgered").await;
    let signer = signer(9);
    let ledger = Arc::new(InMemoryLedger::new());
    let id = asset_id("ledgered");
    append(&ledger, &signer, custody("u1", &id)).await;
    append(&ledger, &signer, indexed("u1", &id)).await;

    let context = h.context.clone().with_ledger(ledger, keys(&signer));
    let report = scrub(&context, Depth::Structural).await.expect("a pass");
    assert!(report.is_clean(), "found {:?}", report.findings);
    assert!(!report.ledger_links_only);
}

#[tokio::test]
async fn a_ledgered_write_without_its_row_is_reported_once_and_a_refused_one_is_not() {
    let h = Harness::new();
    let signer = signer(9);
    let ledger = Arc::new(InMemoryLedger::new());
    let lost = AssetId::new("lost-asset");
    let refused = AssetId::new("refused-asset");
    append(&ledger, &signer, custody("u1", &lost)).await;
    append(&ledger, &signer, indexed("u1", &lost)).await;
    append(&ledger, &signer, custody("u2", &lost)).await;
    append(&ledger, &signer, custody("u3", &refused)).await;
    append(
        &ledger,
        &signer,
        LedgerEvent::Refused {
            upload_id: "u3".to_owned(),
            asset_id: refused.as_str().to_owned(),
            reason: "conflict".to_owned(),
        },
    )
    .await;
    let before = h.snapshot().await;

    let context = h.context.clone().with_ledger(ledger, keys(&signer));
    let report = scrub(&context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.findings,
        vec![Finding::LedgerEntryUnindexed {
            owner: OwnerId::new("scrub-owner"),
            ledger_seq: 1,
            asset: lost,
        }]
    );
    assert_eq!(h.snapshot().await, before, "the scrub changed the store");
}

#[tokio::test]
async fn a_broken_ledger_is_reported_and_nothing_after_the_break_is_trusted() {
    let h = Harness::new();
    let signer = signer(9);
    let owner = OwnerId::new("scrub-owner");
    let first = AssetId::new("first");
    let after = AssetId::new("after-the-break");
    // The second entry is signed by a key this server never published: a forgery.
    let ledger = Arc::new(InMemoryLedger::new());
    append(&ledger, &signer, custody("u1", &first)).await;
    append(&ledger, &self::signer(3), custody("u2", &after)).await;

    let context = h.context.clone().with_ledger(ledger.clone(), keys(&signer));
    let report = scrub(&context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.findings.first(),
        Some(&Finding::LedgerBroken {
            owner,
            verified: 1,
            fault: ChainFault::UnknownKey { seq: 2 },
        })
    );
    // `first` has no row and is inside the verified prefix; `after` is past the break.
    assert_eq!(report.count("ledger_entry_unindexed"), 1);
}

#[tokio::test]
async fn without_keys_the_check_verifies_links_and_says_so() {
    let h = Harness::new();
    let context = h
        .context
        .clone()
        .with_ledger(Arc::new(InMemoryLedger::new()), LedgerKeys::default());
    let report = scrub(&context, Depth::Structural).await.expect("a pass");
    assert!(report.ledger_links_only);
    assert!(report.is_clean());
}
