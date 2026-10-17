//! The one suite every [`ReceiptLog`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **The chain is strictly monotonic and linked.** Each receipt sits one position after its
//!   predecessor and names it by the hash of its signed bytes; the first names none.
//! - **Concurrent issues never fork it.** Two finalizations racing for a position get two
//!   positions, each linked to the one before — the allocation is inside the signed core, so a
//!   fork would be two validly-signed halves.
//! - **One custody event, one receipt.** Re-issuing for an upload answers the receipt already
//!   issued, and the chain does not advance.
//! - **What is read back is what was signed.** `for_upload` and `for_asset` return receipts
//!   byte-identical to the ones `issue` returned, and they still verify under the signer.
//! - **Lookups are scoped.** An asset's receipts are its own, in chain order; an unknown upload
//!   or asset has none.
//!
//! Append-only is a property of the port's shape — no method replaces or removes — and is not a
//! case here. A durable adapter may add a structural check of its own beside its suite.
//!
//! # Reusing a harness
//!
//! Every case scopes its own uploads and assets, so cases may share one log and [`run_all`]
//! does. Positions are therefore asserted relative to each other, except in
//! [`the_first_receipt_opens_the_chain`], which [`run_all`] runs first.

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::receipts::CustodyReceipt;

use super::{
    LocalAttestationKey, RECEIPT_VERSION, ReceiptDraft, ReceiptLog, ReceiptSigner as _, chain_hash,
};
use crate::store::{AssetId, StoreError, UploadId};

/// The log under test.
pub trait Harness: Send + Sync {
    /// The receipt log under test.
    fn receipts(&self) -> &dyn ReceiptLog;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming receipt log must succeed at {doing}: {error}"),
    }
}

/// A fresh attestation key.
fn key() -> LocalAttestationKey {
    LocalAttestationKey::new("capsule.test", HybridSigningKey::generate())
}

/// `case`'s asset `name`.
fn asset(case: &str, name: &str) -> AssetId {
    AssetId::new(format!("{case}-{name}"))
}

/// `case`'s upload `name`.
fn upload(case: &str, name: &str) -> UploadId {
    UploadId::new(format!("{case}-upload-{name}"))
}

/// A draft for `case`'s upload `name` into asset `asset_name`, over `bytes`.
fn draft(case: &str, name: &str, asset_name: &str, bytes: &[u8]) -> ReceiptDraft {
    ReceiptDraft {
        crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
        protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
        upload_id: upload(case, name),
        asset_id: asset(case, asset_name),
        blob_role: "original".to_owned(),
        ciphertext_hash: hash_bytes(bytes),
        size: bytes.len() as u64,
        envelope_hash: Some(hash_bytes(format!("{case}/{name}/envelope").as_bytes())),
        uploaded_by_user: "01937b7c-0000-7000-8000-000000000001".to_owned(),
        uploaded_by_device: Some(format!("{case}-device")),
        received_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

/// Issue `draft` under `key`, expecting it to work.
async fn issue(h: &dyn Harness, draft: ReceiptDraft, key: &LocalAttestationKey) -> CustodyReceipt {
    ok(h.receipts().issue(draft, key).await, "issue a receipt")
}

/// Assert `next` is the receipt immediately after `prior` in the chain.
#[track_caller]
fn follows(next: &CustodyReceipt, prior: &CustodyReceipt) {
    assert_eq!(
        next.core.receipt_seq,
        prior.core.receipt_seq + 1,
        "a receipt sits one position after its predecessor"
    );
    assert_eq!(
        next.core.prior_receipt_hash,
        Some(chain_hash(prior)),
        "a receipt names its predecessor by the hash of its signed bytes"
    );
}

// ===========================================================================================
// The chain
// ===========================================================================================

/// On an empty log, the first receipt is at position one and names no predecessor.
pub async fn the_first_receipt_opens_the_chain(h: &dyn Harness) {
    let first = issue(h, draft("opens", "1", "a", b"one"), &key()).await;
    assert_eq!(first.core.receipt_seq, 1);
    assert_eq!(
        first.core.prior_receipt_hash, None,
        "the first receipt has no predecessor, and an absent field says so"
    );
}

/// Successive receipts are consecutive and each names the one before it.
pub async fn the_chain_is_monotonic_and_each_entry_names_its_predecessor(h: &dyn Harness) {
    let case = "chain";
    let signer = key();
    let first = issue(h, draft(case, "1", "asset", b"one"), &signer).await;
    let second = issue(h, draft(case, "2", "asset", b"two"), &signer).await;
    let third = issue(h, draft(case, "3", "other", b"three"), &signer).await;
    follows(&second, &first);
    follows(&third, &second);
}

/// Issues racing each other each get their own position, and the chain stays linked.
pub async fn concurrent_issues_do_not_fork_the_chain(h: &dyn Harness) {
    let case = "race";
    let signer = key();
    let (a, b, c, d) = tokio::join!(
        h.receipts().issue(draft(case, "a", "asset", b"a"), &signer),
        h.receipts().issue(draft(case, "b", "asset", b"b"), &signer),
        h.receipts().issue(draft(case, "c", "asset", b"c"), &signer),
        h.receipts().issue(draft(case, "d", "asset", b"d"), &signer),
    );
    let mut issued = vec![
        ok(a, "issue a"),
        ok(b, "issue b"),
        ok(c, "issue c"),
        ok(d, "issue d"),
    ];
    issued.sort_by_key(|receipt| receipt.core.receipt_seq);
    for pair in issued.windows(2) {
        follows(&pair[1], &pair[0]);
    }
    // And the log holds exactly those four, in that order.
    let held = ok(
        h.receipts().for_asset(&asset(case, "asset")).await,
        "read the asset's receipts",
    );
    assert_eq!(held, issued);
}

// ===========================================================================================
// One custody event, one receipt
// ===========================================================================================

/// Re-issuing for an upload answers the receipt already issued and does not advance the chain.
pub async fn a_reissued_upload_returns_the_same_receipt(h: &dyn Harness) {
    let case = "reissue";
    let signer = key();
    let first = issue(h, draft(case, "1", "asset", b"one"), &signer).await;
    // A retry whose draft differs is still the same custody event: the first receipt stands.
    let again = issue(h, draft(case, "1", "asset", b"different bytes"), &key()).await;
    assert_eq!(
        first, again,
        "a retried finalization must not mint a second receipt for one custody event"
    );
    let held = ok(
        h.receipts().for_asset(&asset(case, "asset")).await,
        "read the asset's receipts",
    );
    assert_eq!(held, vec![first.clone()], "the retry wrote nothing");
    let next = issue(h, draft(case, "2", "asset", b"two"), &signer).await;
    follows(&next, &first);
}

// ===========================================================================================
// What is read back
// ===========================================================================================

/// The receipt carries what the server established and verifies under the key it names.
pub async fn the_receipt_carries_the_draft_and_verifies_under_its_signer(h: &dyn Harness) {
    let case = "facts";
    let signer = key();
    let drafted = draft(case, "1", "asset", b"the bytes");
    let receipt = issue(h, drafted.clone(), &signer).await;
    let core = &receipt.core;
    assert_eq!(core.version, RECEIPT_VERSION);
    assert_eq!(core.crypto_suite_id, drafted.crypto_suite_id);
    assert_eq!(core.protocol_version, drafted.protocol_version);
    assert_eq!(core.server_id, signer.server_id());
    assert_eq!(core.server_key_id, signer.key_id());
    assert_eq!(core.upload_id, drafted.upload_id.as_str());
    assert_eq!(core.asset_id, drafted.asset_id.as_str());
    assert_eq!(core.blob_role, drafted.blob_role);
    assert_eq!(core.ciphertext_hash, drafted.ciphertext_hash);
    assert_eq!(core.size, drafted.size);
    assert_eq!(core.envelope_hash, drafted.envelope_hash);
    assert_eq!(core.uploaded_by_user, drafted.uploaded_by_user);
    assert_eq!(core.uploaded_by_device, drafted.uploaded_by_device);
    assert_eq!(core.received_at, drafted.received_at);
    assert!(receipt.verify_under(&signer.verifying_key()));
    assert!(
        !receipt.verify_under(&key().verifying_key()),
        "another key does not verify it"
    );
}

/// Both lookups return receipts byte-identical to what `issue` returned, still verifying.
pub async fn a_receipt_reads_back_byte_for_byte(h: &dyn Harness) {
    let case = "readback";
    let signer = key();
    let issued = issue(h, draft(case, "1", "asset", b"one"), &signer).await;
    let by_upload = ok(
        h.receipts().for_upload(&upload(case, "1")).await,
        "read an upload's receipt",
    )
    .expect("the receipt is held");
    assert_eq!(by_upload.to_canonical_cbor(), issued.to_canonical_cbor());
    assert!(by_upload.verify_under(&signer.verifying_key()));
    let by_asset = ok(
        h.receipts().for_asset(&asset(case, "asset")).await,
        "read an asset's receipts",
    );
    assert_eq!(by_asset.len(), 1);
    assert_eq!(by_asset[0].to_canonical_cbor(), issued.to_canonical_cbor());
}

/// An asset's receipts are its own, in chain order; another asset's are not among them.
pub async fn receipts_are_found_by_upload_and_by_asset(h: &dyn Harness) {
    let case = "lookup";
    let signer = key();
    let a1 = issue(h, draft(case, "1", "a", b"one"), &signer).await;
    let b1 = issue(h, draft(case, "2", "b", b"two"), &signer).await;
    let a2 = issue(h, draft(case, "3", "a", b"three"), &signer).await;
    assert_eq!(
        ok(h.receipts().for_asset(&asset(case, "a")).await, "read a"),
        vec![a1, a2],
        "an asset's receipts, in chain order, and only its own"
    );
    assert_eq!(
        ok(h.receipts().for_asset(&asset(case, "b")).await, "read b"),
        vec![b1.clone()]
    );
    assert_eq!(
        ok(
            h.receipts().for_upload(&upload(case, "2")).await,
            "read an upload's receipt"
        ),
        Some(b1)
    );
}

/// An upload or asset nothing was issued for has no receipt.
pub async fn an_unknown_upload_or_asset_has_no_receipt(h: &dyn Harness) {
    let case = "unknown";
    assert_eq!(
        ok(
            h.receipts().for_upload(&upload(case, "never")).await,
            "read an unknown upload"
        ),
        None
    );
    assert!(
        ok(
            h.receipts().for_asset(&asset(case, "never")).await,
            "read an unknown asset"
        )
        .is_empty()
    );
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order. The log must start empty.
pub async fn run_all(h: &dyn Harness) {
    the_first_receipt_opens_the_chain(h).await;
    the_chain_is_monotonic_and_each_entry_names_its_predecessor(h).await;
    concurrent_issues_do_not_fork_the_chain(h).await;
    a_reissued_upload_returns_the_same_receipt(h).await;
    the_receipt_carries_the_draft_and_verifies_under_its_signer(h).await;
    a_receipt_reads_back_byte_for_byte(h).await;
    receipts_are_found_by_upload_and_by_asset(h).await;
    an_unknown_upload_or_asset_has_no_receipt(h).await;
}

#[cfg(test)]
mod tests {
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
}
