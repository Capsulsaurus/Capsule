//! The parser's contract: a manifest decodes, everything else is a typed refusal, nothing panics.

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore};

use super::*;

/// A really signed `create` manifest.
fn core() -> ManifestCore {
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.to_owned(),
        crypto_suite_id: 1,
        protocol_version: "2026-01-01".to_owned(),
        file_id: uuid::Uuid::from_u128(1),
        album_id: uuid::Uuid::from_u128(2),
        amk_version: AmkVersion(0),
        ciphertext_hash: hash_bytes(b"original"),
        plaintext_size: 1024,
        chunk_size: 65_536,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(hash_bytes(b"metadata")),
        created_by_user: uuid::Uuid::from_u128(7),
        created_by_device: uuid::Uuid::from_u128(8),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        action: Action::Create,
        prior_provenance_hash: None,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: None,
    }
}

fn encode(core: ManifestCore) -> Vec<u8> {
    let device = HybridSigningKey::from_seed64(&[1; 64]);
    let write_tier = HybridSigningKey::from_seed64(&[2; 64]);
    let manifest = core.sign(&device, &write_tier).expect("a manifest signs");
    capsule_core::cbor::to_canonical_vec(&manifest).expect("a manifest encodes")
}

#[test]
fn a_signed_manifest_parses_and_is_well_formed() {
    let bytes = encode(core());
    let manifest = parse_valid(&bytes).expect("a well-formed manifest parses");
    assert_eq!(manifest.core, core());
}

#[test]
fn bytes_that_are_not_a_manifest_are_undecodable() {
    assert_eq!(parse(b""), Err(ManifestFault::Undecodable));
    assert_eq!(parse(b"not cbor at all"), Err(ManifestFault::Undecodable));
    let other = capsule_core::cbor::to_canonical_vec(&"a string").expect("encodes");
    assert_eq!(parse(&other), Err(ManifestFault::Undecodable));
}

#[test]
fn a_manifest_over_the_ceiling_is_refused_before_it_is_decoded() {
    let size = usize::try_from(MAX_MANIFEST_BYTES).expect("fits") + 1;
    assert_eq!(
        parse(&vec![0_u8; size]),
        Err(ManifestFault::TooLarge { size: size as u64 })
    );
}

#[test]
fn a_manifest_that_breaks_a_presence_rule_decodes_but_is_not_valid() {
    // A non-`create` with no predecessor: decodable, which the scrub's chain walk needs, and
    // not replayable, which a rebuild needs.
    let mut broken = core();
    broken.action = Action::MetadataUpdate;
    let bytes = encode(broken);
    let manifest = parse(&bytes).expect("it decodes");
    assert_eq!(check_structure(&manifest), Err(ManifestFault::Structural));
    assert_eq!(parse_valid(&bytes), Err(ManifestFault::Structural));
}

#[test]
fn every_truncation_and_bit_flip_of_a_manifest_is_answered_without_a_panic() {
    // The seeded slice of the fuzz target, run on every test pass.
    let bytes = encode(core());
    for len in 0..bytes.len() {
        let _ = parse_valid(&bytes[..len]);
    }
    for at in 0..bytes.len() {
        for bit in [0x01_u8, 0x80] {
            let mut mutated = bytes.clone();
            mutated[at] ^= bit;
            let _ = parse_valid(&mutated);
        }
    }
}

#[test]
fn each_fault_has_a_stable_name() {
    assert_eq!(ManifestFault::TooLarge { size: 1 }.as_str(), "too_large");
    assert_eq!(ManifestFault::Undecodable.as_str(), "undecodable");
    assert_eq!(ManifestFault::Structural.as_str(), "structural");
}
