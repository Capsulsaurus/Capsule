use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::receipts::{
    BlobRole as ReceiptRole, CustodyReceipt, CustodyReceiptCore, role_str,
};

/// This crate can construct, sign and verify a custody receipt from **core's own type**
/// while linking `capsule-core` with `default-features = false`.
///
/// The guard, not a feature. Nothing here uses receipts yet, and the point
/// is that the day it does, it must not have to define its own copy. A signed structure
/// defined at both ends is one added field away from a signature that stops verifying, and
/// the failure would present as the server withholding receipts, which is exactly the
/// accusation custody receipts exist to make checkable. If somebody moves the type back
/// behind `native`, this stops compiling.
#[test]
fn the_server_can_sign_a_receipt_from_cores_own_type() {
    let key = HybridSigningKey::generate();
    let core = CustodyReceiptCore {
        version: "custody-receipt/v1".to_owned(),
        crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
        server_id: "capsule.example".to_owned(),
        server_key_id: Hash32([0x11; 32]),
        receipt_seq: 1,
        prior_receipt_hash: None,
        upload_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f".to_owned(),
        asset_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61".to_owned(),
        blob_role: role_str(ReceiptRole::Original).to_owned(),
        ciphertext_hash: Hash32([0x22; 32]),
        size: 4096,
        envelope_hash: None,
        uploaded_by_user: "01937b7c-0000-7000-8000-000000000001".to_owned(),
        uploaded_by_device: None,
        received_at: "2026-01-01T00:00:00Z".to_owned(),
        unknown: Default::default(),
    };
    let receipt = CustodyReceipt {
        server_sig: key.sign(&core.signing_bytes()),
        core,
    };

    assert!(receipt.verify_under(&key.verifying_key()));
    // And it survives the wire form the client decodes.
    let bytes = receipt.to_canonical_cbor();
    assert_eq!(
        CustodyReceipt::from_canonical_cbor(&bytes).expect("a receipt round-trips"),
        receipt,
    );
}
