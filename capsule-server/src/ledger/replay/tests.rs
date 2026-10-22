//! Custody pairing and key-scoped verification.

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::HybridSigningKey;
use jiff::Timestamp;

use super::*;
use crate::attestation::{LocalAttestationKey, ReceiptSigner as _};
use crate::ledger::{ChainFault, InMemoryLedger, OwnerLedger as _};

fn signer(seed: u8) -> LocalAttestationKey {
    LocalAttestationKey::new("server.test", HybridSigningKey::from_seed64(&[seed; 64]))
}

fn published(signer: &LocalAttestationKey) -> PublishedKey {
    PublishedKey {
        key_id: signer.key_id(),
        public: signer.verifying_key(),
        active_from: Timestamp::UNIX_EPOCH,
        active_to: None,
    }
}

fn custody_event(upload: &str) -> LedgerEvent {
    LedgerEvent::Custody {
        upload_id: upload.to_owned(),
        asset_id: format!("asset-{upload}"),
        blob_role: "metadata".to_owned(),
        content_hash: hash_bytes(upload.as_bytes()),
        size: 7,
        manifest_sha256: None,
        upload_user_id: "user-1".to_owned(),
        uploader_device: None,
        received_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn indexed(upload: &str, sync_seq: Option<u64>) -> LedgerEvent {
    LedgerEvent::Indexed {
        upload_id: upload.to_owned(),
        asset_id: format!("asset-{upload}"),
        sync_seq,
        receipt: None,
    }
}

async fn ledger_of(events: Vec<LedgerEvent>, signer: &LocalAttestationKey) -> LedgerScan {
    let ledger = InMemoryLedger::new();
    let owner = OwnerId::new("owner-1");
    for event in events {
        ledger
            .append(&owner, event, Timestamp::UNIX_EPOCH, signer)
            .await
            .expect("an append lands");
    }
    ledger.scan(&owner).await.expect("a scan reads")
}

#[tokio::test]
async fn each_custody_entry_is_paired_with_what_resolved_it() {
    let key = signer(1);
    let scan = ledger_of(
        vec![
            custody_event("a"),
            indexed("a", Some(3)),
            custody_event("b"),
            LedgerEvent::Refused {
                upload_id: "b".to_owned(),
                asset_id: "asset-b".to_owned(),
                reason: "conflict".to_owned(),
            },
            custody_event("c"),
        ],
        &key,
    )
    .await;

    let found = custody(&scan.entries);
    let resolutions: Vec<_> = found
        .iter()
        .map(|record| (record.upload_id.as_str(), record.resolution))
        .collect();
    assert_eq!(
        resolutions,
        vec![
            ("a", Resolution::Indexed { sync_seq: Some(3) }),
            ("b", Resolution::Refused),
            ("c", Resolution::Unresolved),
        ]
    );
    assert_eq!(found[0].ledger_seq, 1);
    assert_eq!(found[0].content_hash, hash_bytes(b"a"));
}

#[tokio::test]
async fn a_resolution_before_its_custody_entry_resolves_nothing() {
    let key = signer(1);
    let scan = ledger_of(vec![indexed("a", Some(1)), custody_event("a")], &key).await;
    assert_eq!(custody(&scan.entries)[0].resolution, Resolution::Unresolved);
}

#[tokio::test]
async fn a_retried_custody_entry_is_resolved_by_the_one_resolution_after_both() {
    let key = signer(1);
    let scan = ledger_of(
        vec![
            custody_event("a"),
            custody_event("a"),
            indexed("a", Some(2)),
        ],
        &key,
    )
    .await;
    let found = custody(&scan.entries);
    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .all(|record| record.resolution == Resolution::Indexed { sync_seq: Some(2) })
    );
}

#[tokio::test]
async fn keys_check_signatures_and_no_keys_check_links_only() {
    let key = signer(1);
    let owner = OwnerId::new("owner-1");
    let scan = ledger_of(vec![custody_event("a"), indexed("a", Some(1))], &key).await;

    let with = LedgerKeys::new(vec![published(&key)]);
    assert!(with.checks_signatures());
    let report = with.verify(&owner, &scan);
    assert_eq!((report.verified, report.fault), (2, None));

    // A key this server never published: the signatures cannot be trusted.
    let wrong = LedgerKeys::new(vec![published(&signer(2))]);
    let report = wrong.verify(&owner, &scan);
    assert_eq!(report.verified, 0);
    assert!(matches!(
        report.fault,
        Some(ChainFault::UnknownKey { seq: 1 })
    ));

    // No key at all: the links hold, so the chain verifies as far as links can say.
    let none = LedgerKeys::default();
    assert!(!none.checks_signatures());
    let report = none.verify(&owner, &scan);
    assert_eq!((report.verified, report.fault), (2, None));
}

#[tokio::test]
async fn links_alone_still_catch_a_removed_entry() {
    let key = signer(1);
    let owner = OwnerId::new("owner-1");
    let mut scan = ledger_of(
        vec![
            custody_event("a"),
            indexed("a", Some(1)),
            custody_event("b"),
        ],
        &key,
    )
    .await;
    scan.entries.remove(1);
    let report = LedgerKeys::default().verify(&owner, &scan);
    assert_eq!(report.verified, 1);
    assert_eq!(
        report.fault,
        Some(ChainFault::SequenceGap {
            expected: 2,
            found: 3
        })
    );
}
