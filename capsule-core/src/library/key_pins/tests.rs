use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::HybridSigningKey;
use crate::crypto::receipts::{
    BlobRole, CustodyReceipt, CustodyReceiptCore, ReceiptExpectations, verify_receipt,
};

const ORIGIN: &str = "https://capsule.example";
const OTHER: &str = "https://other.example:8443";

fn signing_key(seed: u8) -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[seed; 64])
}

fn key(seed: u8) -> HybridVerifyingKey {
    signing_key(seed).verifying_key()
}

fn bytes(keys: &[HybridVerifyingKey]) -> Vec<Vec<u8>> {
    keys.iter().map(HybridVerifyingKey::to_bytes).collect()
}

fn pinned(root: &Path, origin: &str) -> Option<Vec<Vec<u8>>> {
    load_pinned_attestation_keys(root, origin)
        .expect("the pin reads")
        .map(|keys| bytes(&keys))
}

#[test]
fn first_contact_pins_the_published_history() {
    let dir = TempDir::new().unwrap();
    assert_eq!(pinned(dir.path(), ORIGIN), None, "nothing pinned yet");

    let published = [key(1), key(2)];
    let result = pin_attestation_keys(dir.path(), ORIGIN, &published).unwrap();

    assert_eq!(result.outcome, KeyPinOutcome::FirstContact { keys: 2 });
    assert_eq!(bytes(&result.keys), bytes(&published));
    assert_eq!(pinned(dir.path(), ORIGIN), Some(bytes(&published)));
    assert!(attestation_key_pins_path(dir.path()).is_file());
}

#[test]
fn the_same_history_again_is_unchanged_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let published = [key(1), key(2)];
    pin_attestation_keys(dir.path(), ORIGIN, &published).unwrap();
    let path = attestation_key_pins_path(dir.path());
    let before = fs::read(&path).unwrap();

    let result = pin_attestation_keys(dir.path(), ORIGIN, &published).unwrap();

    assert_eq!(result.outcome, KeyPinOutcome::Unchanged);
    assert_eq!(bytes(&result.keys), bytes(&published));
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn an_appended_key_is_accepted_and_re_pinned() {
    let dir = TempDir::new().unwrap();
    pin_attestation_keys(dir.path(), ORIGIN, &[key(1)]).unwrap();

    let result = pin_attestation_keys(dir.path(), ORIGIN, &[key(1), key(2)]).unwrap();

    assert_eq!(result.outcome, KeyPinOutcome::Extended { added: 1 });
    assert_eq!(bytes(&result.keys), bytes(&[key(1), key(2)]));
    assert_eq!(pinned(dir.path(), ORIGIN), Some(bytes(&[key(1), key(2)])));
}

#[test]
fn a_reordered_history_drops_nothing_and_the_pin_keeps_its_order() {
    let dir = TempDir::new().unwrap();
    pin_attestation_keys(dir.path(), ORIGIN, &[key(1), key(2)]).unwrap();

    let result = pin_attestation_keys(dir.path(), ORIGIN, &[key(3), key(2), key(1)]).unwrap();

    assert_eq!(result.outcome, KeyPinOutcome::Extended { added: 1 });
    assert_eq!(
        pinned(dir.path(), ORIGIN),
        Some(bytes(&[key(1), key(2), key(3)]))
    );
}

#[test]
fn a_replaced_key_refuses_and_leaves_the_pin_untouched() {
    let dir = TempDir::new().unwrap();
    pin_attestation_keys(dir.path(), ORIGIN, &[key(1), key(2)]).unwrap();
    let before = fs::read(attestation_key_pins_path(dir.path())).unwrap();

    let error = pin_attestation_keys(dir.path(), ORIGIN, &[key(1), key(9)]).unwrap_err();

    assert!(
        matches!(
            &error,
            KeyPinError::HistoryRewritten { origin, missing: 1, pinned: 2 } if origin == ORIGIN
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(attestation_key_pins_path(dir.path())).unwrap(),
        before
    );
}

#[test]
fn a_dropped_key_refuses_even_when_another_is_added() {
    let dir = TempDir::new().unwrap();
    pin_attestation_keys(dir.path(), ORIGIN, &[key(1), key(2)]).unwrap();

    for published in [vec![key(2)], vec![key(2), key(3)]] {
        let error = pin_attestation_keys(dir.path(), ORIGIN, &published).unwrap_err();
        assert!(
            matches!(error, KeyPinError::HistoryRewritten { missing: 1, .. }),
            "{error:?}"
        );
    }
    assert_eq!(pinned(dir.path(), ORIGIN), Some(bytes(&[key(1), key(2)])));
}

#[test]
fn an_empty_history_is_refused_and_pins_nothing() {
    let dir = TempDir::new().unwrap();

    let error = pin_attestation_keys(dir.path(), ORIGIN, &[]).unwrap_err();

    assert!(
        matches!(error, KeyPinError::EmptyHistory { .. }),
        "{error:?}"
    );
    assert!(!attestation_key_pins_path(dir.path()).exists());

    pin_attestation_keys(dir.path(), ORIGIN, &[key(1)]).unwrap();
    let error = pin_attestation_keys(dir.path(), ORIGIN, &[]).unwrap_err();
    assert!(
        matches!(error, KeyPinError::EmptyHistory { .. }),
        "{error:?}"
    );
    assert_eq!(pinned(dir.path(), ORIGIN), Some(bytes(&[key(1)])));
}

#[test]
fn each_origin_has_its_own_pin() {
    let dir = TempDir::new().unwrap();
    pin_attestation_keys(dir.path(), ORIGIN, &[key(1)]).unwrap();

    let result = pin_attestation_keys(dir.path(), OTHER, &[key(5)]).unwrap();

    assert_eq!(result.outcome, KeyPinOutcome::FirstContact { keys: 1 });
    assert_eq!(pinned(dir.path(), ORIGIN), Some(bytes(&[key(1)])));
    assert_eq!(pinned(dir.path(), OTHER), Some(bytes(&[key(5)])));
}

/// A receipt the library kept before a rotation still verifies under the history the run
/// verifies under once the rotated-in key is pinned.
#[test]
fn a_receipt_persisted_before_a_rotation_still_verifies() {
    const NOW: i64 = 1_752_105_600;
    let dir = TempDir::new().unwrap();
    let old = signing_key(1);
    let ct = Hash32([0xAB; 32]);
    let core = CustodyReceiptCore {
        version: "custody-receipt/v1".into(),
        crypto_suite_id: 1,
        protocol_version: "2026-07-10".into(),
        server_id: "capsule.example".into(),
        server_key_id: Hash32([0x11; 32]),
        receipt_seq: 1,
        prior_receipt_hash: None,
        upload_id: Uuid::from_u128(9).to_string(),
        asset_id: Uuid::from_u128(101).to_string(),
        blob_role: "original".into(),
        ciphertext_hash: ct,
        size: 4096,
        envelope_hash: None,
        uploaded_by_user: Uuid::from_u128(7).to_string(),
        uploaded_by_device: None,
        received_at: "2026-07-10T00:00:00Z".into(),
        unknown: Default::default(),
    };
    let receipt = CustodyReceipt {
        server_sig: old.sign(&core.signing_bytes()),
        core,
    };
    let expected = ReceiptExpectations {
        ciphertext_hash: ct,
        size: 4096,
        role: BlobRole::Original,
        envelope_hash: None,
    };
    pin_attestation_keys(dir.path(), ORIGIN, &[old.verifying_key()]).unwrap();

    let rotated = pin_attestation_keys(dir.path(), ORIGIN, &[old.verifying_key(), key(2)]).unwrap();

    assert_eq!(rotated.outcome, KeyPinOutcome::Extended { added: 1 });
    assert!(verify_receipt(&receipt, &rotated.keys, &expected, NOW).is_ok());
}

#[test]
fn a_pin_file_of_another_schema_is_refused_not_rewritten() {
    let dir = TempDir::new().unwrap();
    let path = attestation_key_pins_path(dir.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let future = PinFile {
        version: PIN_FILE_VERSION + 1,
        servers: BTreeMap::new(),
    };
    let written = cbor::to_canonical_vec(&future).unwrap();
    fs::write(&path, &written).unwrap();

    let error = pin_attestation_keys(dir.path(), ORIGIN, &[key(1)]).unwrap_err();

    assert!(
        matches!(
            error,
            KeyPinError::UnsupportedVersion {
                found: 2,
                supported: 1
            }
        ),
        "{error:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), written);
}

#[test]
fn a_pinned_key_that_does_not_decode_is_a_typed_error() {
    let dir = TempDir::new().unwrap();
    let mut file = empty_pin_file();
    file.servers
        .insert(ORIGIN.to_owned(), vec![ByteBuf::from(vec![0u8; 3])]);
    store_pin_file(dir.path(), &file).unwrap();

    let error = load_pinned_attestation_keys(dir.path(), ORIGIN).unwrap_err();

    assert!(matches!(error, KeyPinError::CorruptPin { .. }), "{error:?}");
}
