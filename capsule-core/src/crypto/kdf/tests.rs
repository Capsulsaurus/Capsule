use super::*;
use crate::crypto::primitives::info;

#[test]
fn derivation_is_deterministic() {
    let ikm = [7u8; 32];
    let a = derive_key32(&ikm, b"file-123", info::ASSET_FILE_V1);
    let b = derive_key32(&ikm, b"file-123", info::ASSET_FILE_V1);
    assert_eq!(a, b, "same (ikm, salt, info) must produce identical output");
}

#[test]
fn scope_uniqueness_by_salt_and_info() {
    let ikm = [7u8; 32];
    let base = derive_key32(&ikm, b"file-123", info::ASSET_FILE_V1);
    // Different salt (file_id) → different key.
    assert_ne!(base, derive_key32(&ikm, b"file-124", info::ASSET_FILE_V1));
    // Different info label (domain separation) → different key.
    assert_ne!(
        base,
        derive_key32(&ikm, b"file-123", info::METADATA_BLOB_V1)
    );
    // Different ikm (AMK) → different key.
    assert_ne!(
        base,
        derive_key32(&[8u8; 32], b"file-123", info::ASSET_FILE_V1)
    );
}

#[test]
fn truncation_is_a_prefix_of_the_full_expand() {
    // Deriving 32 bytes must equal the first 32 bytes of a 64-byte expansion, so the
    // 512→256 truncation is well-defined and stable across platforms.
    let ikm = [3u8; 32];
    let k32 = derive_key32(&ikm, b"album-1", info::ASSET_FILE_V1);
    let k64 = hkdf_sha512(&ikm, b"album-1", info::ASSET_FILE_V1, 64);
    assert_eq!(&k32[..], &k64[..32]);
}

#[test]
fn golden_vector_pins_the_wiring() {
    // Pins the exact HKDF-SHA512 construction (extract+expand, label, salt) so an
    // accidental algorithm/label change is caught. Cross-checked once on first run.
    let key = derive_key32(&[0u8; 32], b"salt", info::ASSET_FILE_V1);
    assert_eq!(
        hex::encode(key),
        "cb54d8d1556baf00a1fb03103adaa63d7cd5fb108a4e418d76b5a5d724f75587"
    );
}
