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

/// After a restart over a ledger cut at an entry boundary, appends are refused rather than
/// extending the cut chain, until the anchor is reset (as `rebuild --apply` does); and a rewrite
/// at the anchored position is refused the same way.
#[tokio::test]
async fn a_ledger_behind_or_against_its_anchor_takes_no_appends_until_reanchored() {
    let index = Arc::new(InMemoryAssetIndex::new());
    let inner = Arc::new(InMemoryLedger::new());
    let owner = OwnerId::new("owner-1");
    let signer = signer();
    let before = AnchoredLedger::new(inner.clone(), index.clone());
    for version in 1..=3 {
        before
            .append(&owner, directory(version), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("an append lands");
    }

    // The tail is cut after the first entry, exactly at a frame boundary.
    let bytes = inner.raw(&owner);
    let first = crate::ledger::decode_frames(&bytes).entries[0].clone();
    inner.replace_raw(&owner, crate::ledger::encode_frame(&first));

    // A restarted process: a fresh decorator, which has checked nobody.
    let after = AnchoredLedger::new(inner.clone(), index.clone());
    let refused = after
        .append(&owner, directory(4), Timestamp::UNIX_EPOCH, &signer)
        .await;
    assert!(
        matches!(
            refused,
            Err(LedgerError::Damaged {
                fault: ChainFault::BehindAnchor {
                    found: 1,
                    anchored: 3
                },
                ..
            })
        ),
        "a cut chain was extended: {refused:?}"
    );
    assert_eq!(
        inner.scan(&owner).await.expect("scan").entries.len(),
        1,
        "the refusal appended anyway"
    );

    // Re-anchored at what is left, the next append lands and moves the anchor.
    let left = LedgerHead {
        seq: 1,
        hash: first.chain_hash(),
    };
    index
        .reset_ledger_head(&owner, Some(left))
        .await
        .expect("reset");
    let entry = after
        .append(&owner, directory(4), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("an append lands once re-anchored");
    assert_eq!(entry.core.seq, 2);

    // A rewrite: the anchor names another entry at a position the ledger holds.
    index
        .reset_ledger_head(
            &owner,
            Some(LedgerHead {
                seq: 2,
                hash: capsule_core::crypto::hash::hash_bytes(b"another entry"),
            }),
        )
        .await
        .expect("reset");
    let restarted = AnchoredLedger::new(inner.clone(), index.clone());
    let refused = restarted
        .append(&owner, directory(5), Timestamp::UNIX_EPOCH, &signer)
        .await;
    assert!(
        matches!(
            refused,
            Err(LedgerError::Damaged {
                fault: ChainFault::AnchorMismatch { seq: 2 },
                ..
            })
        ),
        "a rewritten chain was extended: {refused:?}"
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

/// The server and an operator's `purge --apply` (#767): two processes, each with its own anchored
/// filesystem ledger over one blob root and one index. Alternating appends stay on one chain,
/// neither process's first append is refused, and the anchor ends at the last entry.
#[tokio::test]
async fn two_anchored_writers_on_one_root_keep_one_chain_and_one_anchor() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let index = Arc::new(InMemoryAssetIndex::new());
    let open = || async {
        AnchoredLedger::new(
            Arc::new(
                crate::ledger::FilesystemLedger::open(root.path())
                    .await
                    .expect("open"),
            ),
            index.clone(),
        )
    };
    let server = open().await;
    let purge = open().await;
    let owner = OwnerId::new("owner-1");
    let signer = signer();

    let mut last = None;
    for version in 1..=6_u64 {
        let writer = if version % 2 == 1 { &server } else { &purge };
        let entry = writer
            .append(&owner, directory(version), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("neither writer is refused");
        assert_eq!(entry.core.seq, version);
        last = Some(LedgerHead {
            seq: entry.core.seq,
            hash: entry.chain_hash(),
        });
    }

    let scan = server.scan(&owner).await.expect("scan");
    let report = verify_links(
        &owner,
        &scan,
        index.ledger_head(&owner).await.expect("read"),
    );
    assert_eq!(report.fault, None, "one chain, matching its anchor");
    assert_eq!(report.verified, 6);
    assert_eq!(index.ledger_head(&owner).await.expect("read"), last);
}
