//! The one suite every [`OwnerLedger`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **Each owner's chain opens at 1 and is linked.** Every entry sits one position after its
//!   predecessor and names it by hash; the first names none; owners do not share a chain.
//! - **What is read back is what was signed**, and it verifies under the signer.
//! - **Concurrent appends never fork a chain.**
//! - **Tampering is detected**: an edited entry, a removed one, a dropped head, a torn tail, and —
//!   against an anchor — a tail cut at an entry boundary.
//! - **A torn tail is cut before the next append; any other damage refuses appends.** An intact
//!   entry this server cannot read is never a torn tail, so it refuses appends and is not cut.
//! - **An owner identifier that cannot name a ledger is refused** by every operation.
//!
//! Tampering goes through [`Harness::raw`] and [`Harness::replace_raw`], the two things a suite
//! cannot do through the port. Both adapters keep the same frames, so a case means the same
//! thing on each.
//!
//! Every case scopes its owners to itself, so cases may share one harness and [`run_all`] does.

use capsule_core::cbor::to_canonical_vec;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use jiff::Timestamp;
use serde::Serialize;

use super::{
    ChainFault, LedgerEntryCore, LedgerError, LedgerEvent, LedgerFuture, LedgerHead, OwnerLedger,
    encode_frame, sign_entry, verify_chain,
};
use crate::attestation::{LocalAttestationKey, ReceiptSigner as _};
use crate::store::OwnerId;

/// The ledger under test, plus raw access for tampering.
pub trait Harness: Send + Sync {
    /// The ledger under test.
    fn ledger(&self) -> &dyn OwnerLedger;

    /// `owner`'s ledger as one byte stream, every segment in order.
    fn raw<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, Vec<u8>>;

    /// Replace `owner`'s ledger with `bytes`, and make the adapter forget what it cached, as a
    /// restart would.
    fn replace_raw<'a>(&'a self, owner: &'a OwnerId, bytes: Vec<u8>) -> LedgerFuture<'a, ()>;
}

/// Unwrap a ledger result.
#[track_caller]
fn ok<T>(result: Result<T, LedgerError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming owner ledger must succeed at {doing}: {error}"),
    }
}

/// A fresh attestation key.
fn key() -> LocalAttestationKey {
    LocalAttestationKey::new("capsule.test", HybridSigningKey::generate())
}

/// A resolver that knows exactly `key`.
fn resolver(key: &LocalAttestationKey) -> impl Fn(&Hash32) -> Option<HybridVerifyingKey> + '_ {
    move |id| (*id == key.key_id()).then(|| key.verifying_key())
}

/// `case`'s owner `name`.
fn owner(case: &str, name: &str) -> OwnerId {
    OwnerId::new(format!("{case}-{name}"))
}

/// A distinct custody event.
fn custody(tag: u8) -> LedgerEvent {
    LedgerEvent::Custody {
        upload_id: format!("upload-{tag}"),
        asset_id: format!("asset-{tag}"),
        blob_role: "original".to_owned(),
        content_hash: Hash32::from_bytes([tag; 32]),
        size: u64::from(tag) * 100,
        manifest_sha256: None,
        upload_user_id: "uploader".to_owned(),
        uploader_device: Some("device".to_owned()),
        received_at: "2026-10-22T00:00:00Z".to_owned(),
    }
}

/// Append `count` custody events to `owner`'s ledger.
async fn fill(
    harness: &dyn Harness,
    owner: &OwnerId,
    signer: &LocalAttestationKey,
    count: u8,
) -> Vec<super::LedgerEntry> {
    let mut appended = Vec::new();
    for tag in 0..count {
        appended.push(ok(
            harness
                .ledger()
                .append(owner, custody(tag), Timestamp::UNIX_EPOCH, signer)
                .await,
            "append",
        ));
    }
    appended
}

/// Split a ledger's bytes into its frames.
fn frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut at = 0;
    while at + 4 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let end = (at + 4 + len as usize).min(bytes.len());
        frames.push(bytes[at..end].to_vec());
        at = end;
    }
    frames
}

/// The frame a newer server would write for `core`: signed by `signer` and framed as this
/// server frames an entry, whatever shape `core` has.
pub(crate) fn newer_frame<C: Serialize>(core: &C, signer: &LocalAttestationKey) -> Vec<u8> {
    #[derive(Serialize)]
    struct Newer<'a, C> {
        core: &'a C,
        server_sig: HybridSignature,
    }
    let server_sig = signer.sign(&to_canonical_vec(core).expect("a core serializes"));
    let body = to_canonical_vec(&Newer { core, server_sig }).expect("an entry serializes");
    let mut frame = u32::try_from(body.len())
        .expect("an entry is far smaller than 4 GiB")
        .to_be_bytes()
        .to_vec();
    frame.extend_from_slice(&body);
    frame
}

/// Verify `owner`'s ledger as it now stands.
async fn verify(
    harness: &dyn Harness,
    owner: &OwnerId,
    signer: &LocalAttestationKey,
    anchor: Option<LedgerHead>,
) -> super::ChainReport {
    let scan = ok(harness.ledger().scan(owner).await, "scan");
    verify_chain(owner, &scan, &resolver(signer), anchor)
}

/// The first entry is position 1 and names no predecessor.
pub async fn the_first_entry_opens_the_chain(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("opens", "a");
    let first = fill(harness, &owner, &signer, 1).await.remove(0);
    assert_eq!(first.core.seq, 1);
    assert_eq!(first.core.prior_entry_hash, None);
    assert_eq!(first.core.owner_id, owner.as_str());
    assert_eq!(first.core.server_key_id, signer.key_id());
    assert!(first.verify_under(&signer.verifying_key()));
}

/// Every kind of entry reads back as it was signed, linked in order, and verifies to the end.
pub async fn entries_read_back_as_signed_and_verify_in_order(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("reads-back", "a");
    let events = [
        custody(1),
        LedgerEvent::Indexed {
            upload_id: "upload-1".to_owned(),
            asset_id: "asset-1".to_owned(),
            sync_seq: Some(7),
            receipt: None,
        },
        LedgerEvent::Directory {
            user_id: "uploader".to_owned(),
            directory_version: 3,
            document_hash: Hash32::from_bytes([9; 32]),
        },
        LedgerEvent::Lifecycle {
            asset_id: "asset-1".to_owned(),
            action: "purge".to_owned(),
            manifest_hash: Some(Hash32::from_bytes([4; 32])),
            sync_seq: Some(8),
        },
    ];
    let mut appended = Vec::new();
    for event in events {
        appended.push(ok(
            harness
                .ledger()
                .append(&owner, event, Timestamp::UNIX_EPOCH, &signer)
                .await,
            "append",
        ));
    }

    let scan = ok(harness.ledger().scan(&owner).await, "scan");
    assert_eq!(
        scan.entries, appended,
        "what is read back is what was signed"
    );
    assert_eq!(scan.damage, None);
    for pair in appended.windows(2) {
        assert_eq!(pair[1].core.seq, pair[0].core.seq + 1);
        assert_eq!(pair[1].core.prior_entry_hash, Some(pair[0].chain_hash()));
    }

    let report = verify_chain(&owner, &scan, &resolver(&signer), None);
    assert_eq!(report.fault, None);
    assert_eq!(report.verified, 4);
    assert_eq!(
        report.head,
        Some(LedgerHead {
            seq: 4,
            hash: appended[3].chain_hash()
        })
    );
}

/// Each owner has a chain of its own, and the listing names every owner with an entry.
pub async fn owners_have_separate_chains(harness: &dyn Harness) {
    let signer = key();
    let first = owner("separate", "a");
    let second = owner("separate", "b");
    fill(harness, &second, &signer, 2).await;
    let opened = fill(harness, &first, &signer, 1).await;
    assert_eq!(
        opened[0].core.seq, 1,
        "another owner's entries do not advance this chain"
    );

    let listed = ok(harness.ledger().owners().await, "list owners");
    let position = |owner: &OwnerId| listed.iter().position(|listed| listed == owner);
    let (Some(a), Some(b)) = (position(&first), position(&second)) else {
        panic!("both owners are listed: {listed:?}");
    };
    assert!(a < b, "owners are listed in ascending order");
    assert!(
        ok(
            harness.ledger().scan(&owner("separate", "never")).await,
            "scan"
        )
        .entries
        .is_empty(),
        "an owner with no ledger scans to nothing"
    );
}

/// Appends racing on one owner take distinct, linked positions.
pub async fn concurrent_appends_never_fork_the_chain(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("concurrent", "a");
    let ledger = harness.ledger();
    let append = |tag| ledger.append(&owner, custody(tag), Timestamp::UNIX_EPOCH, &signer);
    let (a, b, c, d) = tokio::join!(append(1), append(2), append(3), append(4));
    let mut seqs: Vec<u64> = [a, b, c, d]
        .into_iter()
        .map(|entry| ok(entry, "a concurrent append").core.seq)
        .collect();
    seqs.sort_unstable();
    assert_eq!(seqs, vec![1, 2, 3, 4]);

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, None, "the raced chain is one chain");
    assert_eq!(report.verified, 4);
}

/// An entry edited after it was written fails its signature, and replay stops before it.
pub async fn an_edited_entry_is_detected(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("edited", "a");
    let mut entries = fill(harness, &owner, &signer, 3).await;

    // Rewrite the middle entry's facts under its original signature: well-formed, linked by
    // nothing it can change, and not what the key signed.
    if let LedgerEvent::Custody { size, .. } = &mut entries[1].core.event {
        *size += 1;
    }
    let forged: Vec<u8> = entries.iter().flat_map(encode_frame).collect();
    ok(harness.replace_raw(&owner, forged).await, "tamper");

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, Some(ChainFault::BadSignature { seq: 2 }));
    assert_eq!(report.verified, 1);
}

/// An entry removed from the middle leaves a gap.
pub async fn a_removed_entry_is_detected(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("removed", "a");
    fill(harness, &owner, &signer, 3).await;
    let mut frames = frames(&ok(harness.raw(&owner).await, "read raw"));
    frames.remove(1);
    ok(harness.replace_raw(&owner, frames.concat()).await, "tamper");

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(
        report.fault,
        Some(ChainFault::SequenceGap {
            expected: 2,
            found: 3
        })
    );
    assert_eq!(report.verified, 1);
}

/// A ledger whose first entry was dropped does not open its chain.
pub async fn a_dropped_head_is_detected(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("dropped-head", "a");
    fill(harness, &owner, &signer, 2).await;
    let frames = frames(&ok(harness.raw(&owner).await, "read raw"));
    ok(
        harness.replace_raw(&owner, frames[1..].concat()).await,
        "tamper",
    );

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(
        report.fault,
        Some(ChainFault::SequenceGap {
            expected: 1,
            found: 2
        })
    );
    assert_eq!(report.verified, 0);
    assert_eq!(report.head, None);
}

/// A tail torn mid-frame is reported, and the next append cuts it and lands in its place.
pub async fn a_torn_tail_is_reported_and_cut_by_the_next_append(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("torn", "a");
    fill(harness, &owner, &signer, 3).await;
    let mut raw = ok(harness.raw(&owner).await, "read raw");
    raw.truncate(raw.len() - 5);
    ok(harness.replace_raw(&owner, raw).await, "tear");

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, Some(ChainFault::TornTail { entries: 2 }));
    assert_eq!(
        report.verified, 2,
        "every whole entry before the tear still verifies"
    );

    let next = ok(
        harness
            .ledger()
            .append(&owner, custody(9), Timestamp::UNIX_EPOCH, &signer)
            .await,
        "append after a tear",
    );
    assert_eq!(next.core.seq, 3, "the torn entry was never an entry");
    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, None);
    assert_eq!(report.verified, 3);
}

/// Zeroes where a crashed append's frame was being written are a torn tail, not damage: the
/// next append cuts them and lands, rather than wedging the owner.
pub async fn a_zero_filled_tail_is_cut_by_the_next_append(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("zero-filled", "a");
    fill(harness, &owner, &signer, 2).await;
    let mut raw = ok(harness.raw(&owner).await, "read raw");
    raw.extend_from_slice(&[0; 512]);
    ok(harness.replace_raw(&owner, raw).await, "crash");

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, Some(ChainFault::TornTail { entries: 2 }));

    let next = ok(
        harness
            .ledger()
            .append(&owner, custody(9), Timestamp::UNIX_EPOCH, &signer)
            .await,
        "append after a zero-filled crash",
    );
    assert_eq!(next.core.seq, 3);
    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, None);
    assert_eq!(report.verified, 3);
}

/// An intact entry this server cannot read — one a newer server wrote, with a field this server
/// does not know — is not a torn tail even at the very end: it is reported, the next append is
/// refused, and its bytes are left exactly where they were.
pub async fn an_entry_from_a_newer_server_is_refused_not_cut(harness: &dyn Harness) {
    #[derive(Serialize)]
    struct WithNewField<'a> {
        #[serde(flatten)]
        core: &'a LedgerEntryCore,
        replicated_to: Vec<String>,
    }

    let signer = key();
    let owner = owner("newer", "a");
    let entries = fill(harness, &owner, &signer, 2).await;
    let head = LedgerHead {
        seq: 2,
        hash: entries[1].chain_hash(),
    };
    let next = sign_entry(
        &owner,
        Some(head),
        custody(9),
        Timestamp::UNIX_EPOCH,
        &signer,
    );
    let newer = newer_frame(
        &WithNewField {
            core: &next.core,
            replicated_to: vec!["peer".to_owned()],
        },
        &signer,
    );
    let mut raw = ok(harness.raw(&owner).await, "read raw");
    raw.extend_from_slice(&newer);
    ok(harness.replace_raw(&owner, raw.clone()).await, "upgrade");

    let report = verify(harness, &owner, &signer, None).await;
    assert_eq!(report.fault, Some(ChainFault::Unrecognised { entries: 2 }));
    assert_eq!(report.verified, 2);

    let refused = harness
        .ledger()
        .append(&owner, custody(10), Timestamp::UNIX_EPOCH, &signer)
        .await;
    assert!(
        matches!(
            refused,
            Err(LedgerError::Damaged {
                fault: ChainFault::Unrecognised { entries: 2 },
                ..
            })
        ),
        "an acknowledged entry is refused, never cut: {refused:?}"
    );
    assert_eq!(
        ok(harness.raw(&owner).await, "read raw"),
        raw,
        "the newer server's entry is still there"
    );
}

/// A tail cut exactly at an entry boundary verifies as a shorter chain, and an anchor catches it.
pub async fn a_tail_cut_at_a_boundary_is_caught_by_an_anchor(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("cut-tail", "a");
    let entries = fill(harness, &owner, &signer, 3).await;
    let anchor = LedgerHead {
        seq: 3,
        hash: entries[2].chain_hash(),
    };
    let frames = frames(&ok(harness.raw(&owner).await, "read raw"));
    ok(
        harness.replace_raw(&owner, frames[..2].concat()).await,
        "cut",
    );

    assert_eq!(
        verify(harness, &owner, &signer, None).await.fault,
        None,
        "without an anchor a boundary cut is a valid shorter chain"
    );
    let report = verify(harness, &owner, &signer, Some(anchor)).await;
    assert_eq!(
        report.fault,
        Some(ChainFault::BehindAnchor {
            found: 2,
            anchored: 3
        })
    );
    assert_eq!(report.verified, 2);
}

/// An entry signed by a key nobody published does not verify.
pub async fn an_unpublished_key_is_detected(harness: &dyn Harness) {
    let signer = key();
    let stranger = key();
    let owner = owner("unpublished", "a");
    fill(harness, &owner, &signer, 1).await;
    let report = verify(harness, &owner, &stranger, None).await;
    assert_eq!(report.fault, Some(ChainFault::UnknownKey { seq: 1 }));
    assert_eq!(report.verified, 0);
}

/// A ledger damaged anywhere but its tail takes no appends.
pub async fn a_damaged_ledger_refuses_appends(harness: &dyn Harness) {
    let signer = key();
    let owner = owner("damaged", "a");
    fill(harness, &owner, &signer, 3).await;
    let mut frames = frames(&ok(harness.raw(&owner).await, "read raw"));
    frames.remove(1);
    ok(harness.replace_raw(&owner, frames.concat()).await, "tamper");

    let refused = harness
        .ledger()
        .append(&owner, custody(9), Timestamp::UNIX_EPOCH, &signer)
        .await;
    assert!(
        matches!(refused, Err(LedgerError::Damaged { .. })),
        "extending a broken chain would bury the break: {refused:?}"
    );
}

/// An owner identifier that cannot name a ledger is refused by every operation.
pub async fn a_malformed_owner_is_refused(harness: &dyn Harness) {
    let signer = key();
    for bad in ["", "..", "../../etc", "a/b", "a.log"] {
        let owner = OwnerId::new(bad);
        assert!(
            matches!(
                harness
                    .ledger()
                    .append(&owner, custody(1), Timestamp::UNIX_EPOCH, &signer)
                    .await,
                Err(LedgerError::MalformedOwner { .. })
            ),
            "{bad:?} must not name a ledger"
        );
        assert!(matches!(
            harness.ledger().scan(&owner).await,
            Err(LedgerError::MalformedOwner { .. })
        ));
    }
}

/// Every case, in one pass on one harness.
pub async fn run_all(harness: &dyn Harness) {
    the_first_entry_opens_the_chain(harness).await;
    entries_read_back_as_signed_and_verify_in_order(harness).await;
    owners_have_separate_chains(harness).await;
    concurrent_appends_never_fork_the_chain(harness).await;
    an_edited_entry_is_detected(harness).await;
    a_removed_entry_is_detected(harness).await;
    a_dropped_head_is_detected(harness).await;
    a_torn_tail_is_reported_and_cut_by_the_next_append(harness).await;
    a_zero_filled_tail_is_cut_by_the_next_append(harness).await;
    an_entry_from_a_newer_server_is_refused_not_cut(harness).await;
    a_tail_cut_at_a_boundary_is_caught_by_an_anchor(harness).await;
    an_unpublished_key_is_detected(harness).await;
    a_damaged_ledger_refuses_appends(harness).await;
    a_malformed_owner_is_refused(harness).await;
}
