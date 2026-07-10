//! Pure key-level unit tests for the custody receipt / storage attestation crypto and the
//! proof-of-loss composition (slice `S-C15`). No database: the persistence-layer atomicity
//! and monotonicity bullets are covered by the upload/media testcontainer suites.

#![allow(clippy::unwrap_used)]

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use jiff::{SignedDuration, Timestamp};

use super::*;

/// A deterministic keyring for `server_id`, seeded by `seed`.
fn keyring(server_id: &str, seed: u8) -> AttestationKeyring {
    let mut s = [0u8; 64];
    s.iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = seed ^ (i as u8));
    AttestationKeyring::new(server_id.to_string(), &s, Vec::new())
}

fn sample_receipt(kr: &AttestationKeyring) -> CustodyReceipt {
    let ciphertext_hash = hash_bytes(b"the ciphertext");
    let core = kr.new_receipt_core(
        "2026-05-31".to_string(),
        1,
        None,
        "upload-1".to_string(),
        "asset-1".to_string(),
        "original".to_string(),
        ciphertext_hash,
        4096,
        Some(hash_bytes(b"envelope")),
        "user-1".to_string(),
        Some("device-1".to_string()),
        Timestamp::now().to_string(),
    );
    kr.sign_receipt(core)
}

fn non_holding_attestation(
    kr: &AttestationKeyring,
    ciphertext_hash: &Hash32,
) -> StorageAttestation {
    let verdict = AttestedVerdict {
        asset_id: "asset-1".to_string(),
        durable: false,
        blobs: vec![AttestedBlob {
            hash: ciphertext_hash.to_hex(),
            role: "original".to_string(),
            stored: false,
            indexed: true,
            retrievable: false,
        }],
        checked_at: Timestamp::now().to_string(),
    };
    kr.attest_verdict(verdict, Some(b"nonce-xyz".to_vec()))
}

#[test]
fn receipt_signature_round_trips_under_its_own_keyring() {
    let kr = keyring("home.tld", 7);
    let receipt = sample_receipt(&kr);
    assert!(
        kr.verify_receipt(&receipt),
        "a freshly signed receipt verifies"
    );
    // The signed core actually covers the fields: a mutated field breaks verification.
    let mut tampered = receipt.clone();
    tampered.core.size += 1;
    assert!(
        !kr.verify_receipt(&tampered),
        "mutating a signed field breaks the signature"
    );
}

#[test]
fn receipt_content_hash_is_stable_and_chains() {
    let kr = keyring("home.tld", 9);
    let receipt = sample_receipt(&kr);
    // Round-trip through canonical CBOR preserves the receipt and its content hash.
    let bytes = receipt.to_canonical_cbor();
    let back = CustodyReceipt::from_canonical_cbor(&bytes).unwrap();
    assert_eq!(back, receipt);
    assert_eq!(back.content_hash(), receipt.content_hash());
}

#[test]
fn cross_server_replay_is_rejected_on_the_identity_binding() {
    // Invariant: server B rejects server A's receipt.
    let server_a = keyring("a.tld", 1);
    let server_b = keyring("b.tld", 2);
    let receipt_a = sample_receipt(&server_a);
    assert!(server_a.verify_receipt(&receipt_a));
    assert!(
        !server_b.verify_receipt(&receipt_a),
        "server B must reject server A's receipt on the server_id/key_id binding"
    );
}

#[test]
fn rotation_continuity_a_pre_rotation_receipt_still_verifies() {
    // Sign under the OLD key, then rotate: the new keyring retains the old public key in its
    // append-only history, so the old receipt still verifies via its server_key_id.
    let old = keyring("home.tld", 3);
    let old_receipt = sample_receipt(&old);
    let old_pub = PublishedKey {
        key_id: old.active_key_id(),
        public: old.history().last().unwrap().public.clone(),
        active_from: Timestamp::UNIX_EPOCH,
        active_to: Some(Timestamp::now()),
    };

    // The rotated keyring has a *different* active key but carries the old one in history.
    let mut new_seed = [0u8; 64];
    new_seed
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = 200 ^ (i as u8));
    let rotated = AttestationKeyring::new("home.tld".to_string(), &new_seed, vec![old_pub]);

    assert_ne!(
        rotated.active_key_id(),
        old.active_key_id(),
        "the key actually rotated"
    );
    assert!(
        rotated.verify_receipt(&old_receipt),
        "a pre-rotation receipt verifies against the published key history"
    );
    // And the well-known publication lists both keys, retired entry included.
    assert_eq!(rotated.well_known().keys.len(), 2);
}

#[test]
fn well_known_round_trips_into_a_verifying_history() {
    let kr = keyring("home.tld", 5);
    let receipt = sample_receipt(&kr);
    let published = kr.well_known();
    // A verifier that pinned only the published document can rebuild the history and verify.
    let history = published.to_history().unwrap();
    let rebuilt = AttestationKeyring::new(
        "home.tld".to_string(),
        // A throwaway active key; verification rides the resolved historical key.
        &[0u8; 64],
        history,
    );
    assert!(rebuilt.verify_receipt(&receipt));
}

#[test]
fn attestation_nonce_is_echoed_and_the_signature_covers_it() {
    let kr = keyring("home.tld", 11);
    let hash = hash_bytes(b"blob");
    let att = non_holding_attestation(&kr, &hash);
    assert_eq!(
        att.core.nonce.as_ref().map(|b| b.to_vec()),
        Some(b"nonce-xyz".to_vec()),
        "the client nonce is echoed verbatim"
    );
    assert!(kr.verify_attestation(&att));

    // Invariant 34: mutating the nonce OR any verdict field breaks verification.
    let mut tampered_nonce = att.clone();
    tampered_nonce.core.nonce = Some(serde_bytes::ByteBuf::from(b"other".to_vec()));
    assert!(
        !kr.verify_attestation(&tampered_nonce),
        "a swapped nonce fails"
    );

    let mut tampered_verdict = att.clone();
    tampered_verdict.core.verdict.durable = true;
    assert!(
        !kr.verify_attestation(&tampered_verdict),
        "a flipped verdict fails"
    );
}

#[test]
fn proof_of_loss_composes_receipt_plus_non_holding_attestation() {
    let kr = keyring("home.tld", 13);
    let receipt = sample_receipt(&kr);
    let att = non_holding_attestation(&kr, &receipt.core.ciphertext_hash);
    let now = Timestamp::now();
    assert_eq!(
        classify_non_holding(&receipt, &att, None, now, &kr),
        NonHolding::Loss,
        "acceptance + signed non-holding, with no rebuttal, is a provable loss"
    );
}

#[test]
fn proof_of_loss_fails_across_servers() {
    let server_a = keyring("a.tld", 1);
    let server_b = keyring("b.tld", 2);
    let receipt_a = sample_receipt(&server_a);
    let att_a = non_holding_attestation(&server_a, &receipt_a.core.ciphertext_hash);
    // Presented to server B's verifier, A's evidence does not compose.
    assert_eq!(
        classify_non_holding(&receipt_a, &att_a, None, Timestamp::now(), &server_b),
        NonHolding::Unproven,
    );
}

#[test]
fn delete_rebuttal_reclassifies_an_elapsed_retention_purge() {
    let kr = keyring("home.tld", 17);
    let receipt = sample_receipt(&kr);
    let att = non_holding_attestation(&kr, &receipt.core.ciphertext_hash);
    let now = Timestamp::now();

    // An elapsed-retention delete for this asset rebuts the loss → authorized purge.
    let elapsed = DeleteRebuttal {
        asset_id: "asset-1".to_string(),
        retention_until: now - SignedDuration::from_hours(1),
    };
    assert_eq!(
        classify_non_holding(&receipt, &att, Some(&elapsed), now, &kr),
        NonHolding::AuthorizedPurge,
    );

    // A not-yet-elapsed retention does NOT rebut — it is still a loss until the window passes.
    let pending = DeleteRebuttal {
        asset_id: "asset-1".to_string(),
        retention_until: now + SignedDuration::from_hours(1),
    };
    assert_eq!(
        classify_non_holding(&receipt, &att, Some(&pending), now, &kr),
        NonHolding::Loss,
    );
}

/// Cross-crate byte-compatibility (slice `S-D4`): a receipt this server signs must verify under
/// the **client's** independent verifier (`capsule_core::library::verify_receipt`) that clients
/// and the SDK use. The two `CustodyReceiptCore` mirrors serialize to byte-identical canonical
/// CBOR, so the client re-derives the exact signing bytes and the hybrid signature verifies —
/// with a matching field check against what the client sent. This is the wire contract the SDK
/// mock smokes assume; here it is proven against the real server-side signer.
#[test]
fn server_signed_receipt_verifies_under_the_client_verifier() {
    use capsule_core::library::{
        CustodyReceipt as ClientReceipt, ReceiptExpectations, ReceiptRejection, verify_receipt,
    };
    use capsule_core::library::BlobRole;

    let kr = keyring("home.tld", 42);
    let receipt = sample_receipt(&kr);
    let ciphertext_hash = receipt.core.ciphertext_hash;
    let envelope_hash = receipt.core.envelope_hash;

    // Serialize on the server, decode with the client's independent type.
    let bytes = receipt.to_canonical_cbor();
    let client_receipt = ClientReceipt::from_canonical_cbor(&bytes).unwrap();

    // The client pins the server's published attestation key (its active key here).
    let pinned = kr.history().last().unwrap().public.clone();
    let now = Timestamp::now().as_second();
    let expected = ReceiptExpectations {
        ciphertext_hash,
        size: 4096,
        role: BlobRole::Original,
        envelope_hash,
    };
    assert!(
        verify_receipt(&client_receipt, &[pinned.clone()], &expected, now).is_ok(),
        "server-signed receipt must verify under the client verifier"
    );

    // A field the client can check but that the server did not attest to → refuse.
    let wrong = ReceiptExpectations {
        size: 4097,
        ..expected.clone()
    };
    assert_eq!(
        verify_receipt(&client_receipt, &[pinned], &wrong, now),
        Err(ReceiptRejection::FieldMismatch("size"))
    );
}

#[test]
fn a_still_holding_attestation_is_not_a_loss() {
    let kr = keyring("home.tld", 19);
    let receipt = sample_receipt(&kr);
    // Attestation reports the blob as fully held.
    let verdict = AttestedVerdict {
        asset_id: "asset-1".to_string(),
        durable: true,
        blobs: vec![AttestedBlob {
            hash: receipt.core.ciphertext_hash.to_hex(),
            role: "original".to_string(),
            stored: true,
            indexed: true,
            retrievable: true,
        }],
        checked_at: Timestamp::now().to_string(),
    };
    let att = kr.attest_verdict(verdict, None);
    assert_eq!(
        classify_non_holding(&receipt, &att, None, Timestamp::now(), &kr),
        NonHolding::Unproven,
        "a durable attestation cannot be spun as a loss proof"
    );
}
