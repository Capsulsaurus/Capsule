use super::*;
use crate::crypto::keys::HybridSigningKey;
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};

fn core(action: Action, prior: Option<Hash32>) -> ManifestCore {
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id: Uuid::from_u128(0xF11E),
        album_id: Uuid::from_u128(0xA1),
        amk_version: AmkVersion(1),
        ciphertext_hash: Hash32([0xCC; 32]),
        plaintext_size: 1024,
        chunk_size: 65_520,
        nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: None,
        created_by_user: Uuid::from_u128(0x05E2),
        created_by_device: Uuid::from_u128(0xD1),
        client_version: "capsule-cli/0.1.0".into(),
        timestamp: "2026-05-31T12:00:00Z".into(),
        action,
        prior_provenance_hash: prior,
        retention_until: None,
        upgraded_from: None,
        unknown: BTreeMap::new(),
    }
}

#[test]
fn sign_produces_two_verifiable_signatures() {
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let m = core(Action::Create, None).sign(&device, &write).unwrap();

    let bytes = m.signing_bytes();
    assert!(device.verifying_key().verify(&bytes, &m.device_sig));
    assert!(write.verifying_key().verify(&bytes, &m.write_sig));
}

#[test]
fn signing_bytes_are_canonical_and_stable() {
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let m = core(Action::Create, None).sign(&device, &write).unwrap();
    // The core round-trips through canonical CBOR unchanged, and the full manifest too.
    let back: AssetManifest = cbor::from_slice(&cbor::to_canonical_vec(&m).unwrap()).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.signing_bytes(), m.signing_bytes());
}

/// A wrapped-file-key length (`structural_ok` is length-agnostic; the real length is
/// pinned by `crate::crypto::encryption::WRAPPED_FILE_KEY_LEN`).
const WRAPPED_LEN: usize = crate::crypto::encryption::WRAPPED_FILE_KEY_LEN;

/// A structurally-valid core for `action` (correct prior placement + `metadata_blob_hash`
/// present exactly on the metadata-bearing actions), so tests can perturb exactly one field.
fn valid_core(action: Action) -> ManifestCore {
    let prior = (!action.is_create()).then_some(Hash32([1; 32]));
    let mut c = core(action, prior);
    c.metadata_blob_hash = action.binds_metadata_blob().then_some(Hash32([0x4D; 32]));
    c
}

#[test]
fn structural_rules_prior_hash_and_retention() {
    // create + null prior: ok.
    assert!(
        valid_core(Action::Create)
            .sign(&dev(), &wt())
            .unwrap()
            .structural_ok()
    );
    // create + non-null prior: violation.
    let mut c = valid_core(Action::Create);
    c.prior_provenance_hash = Some(Hash32([1; 32]));
    assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
    // non-create + null prior: violation.
    let mut c = valid_core(Action::Replace);
    c.prior_provenance_hash = None;
    assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
    // non-create + non-null prior: ok.
    assert!(
        valid_core(Action::Replace)
            .sign(&dev(), &wt())
            .unwrap()
            .structural_ok()
    );

    // retention only on delete.
    let mut c = valid_core(Action::MetadataUpdate);
    c.retention_until = Some("2026-07-01T00:00:00Z".into());
    assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
    let mut d = valid_core(Action::Delete);
    d.retention_until = Some("2026-07-01T00:00:00Z".into());
    assert!(d.sign(&dev(), &wt()).unwrap().structural_ok());
}

#[test]
fn structural_rule_wrapped_file_key_present_iff_wrapped() {
    // derived (default) + absent: ok.
    assert!(
        valid_core(Action::Create)
            .sign(&dev(), &wt())
            .unwrap()
            .structural_ok()
    );
    // wrapped + present: ok.
    let mut c = valid_core(Action::Create);
    c.key_mode = KeyMode::Wrapped;
    c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; WRAPPED_LEN]));
    assert!(c.sign(&dev(), &wt()).unwrap().structural_ok());
    // wrapped + absent: violation.
    let mut c = valid_core(Action::Create);
    c.key_mode = KeyMode::Wrapped;
    c.wrapped_file_key = None;
    assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
    // derived + present: violation.
    let mut c = valid_core(Action::Create);
    c.key_mode = KeyMode::Derived;
    c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; WRAPPED_LEN]));
    assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
}

/// The `metadata_blob_hash` presence-by-action rule: present exactly on
/// `create | replace | metadata-update`, absent on the other four.
#[test]
fn structural_rule_metadata_blob_hash_present_iff_action_binds() {
    for action in [
        Action::Create,
        Action::Replace,
        Action::Delete,
        Action::MetadataUpdate,
        Action::DerivativeAdd,
        Action::DerivativeReplace,
        Action::TrashRestore,
    ] {
        let binds = action.binds_metadata_blob();
        // The action's canonical presence is well-formed.
        assert!(
            valid_core(action)
                .sign(&dev(), &wt())
                .unwrap()
                .structural_ok(),
            "{action:?} canonical presence must be structurally ok"
        );
        // Flip the presence: a metadata-bearing action missing the hash, or a non-bearing
        // action carrying one, is a structural violation.
        let mut c = valid_core(action);
        c.metadata_blob_hash = if binds {
            None
        } else {
            Some(Hash32([0x4D; 32]))
        };
        assert!(
            !c.sign(&dev(), &wt()).unwrap().structural_ok(),
            "{action:?} with flipped metadata_blob_hash presence must be rejected"
        );
    }
}

#[test]
fn derivative_chain_is_independent() {
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let dm = DerivativeCore {
        version: DERIVATIVE_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: Some(PROTOCOL_VERSION.into()),
        amk_version: Some(AmkVersion(1)),
        source_asset_id: Uuid::from_u128(0xF11E),
        role: DerivativeRole::Thumbnail,
        format: "image/avif".into(),
        ciphertext_hash: Hash32([0xAB; 32]),
        nonce_prefix: [9, 8, 7, 6, 5, 4, 3],
        generated_by_device: Uuid::from_u128(0xD1),
        generated_by_client: "capsule-cli/0.1.0".into(),
        model_id: None,
        model_version: None,
        generated_at: "2026-05-31T12:00:00Z".into(),
        prior_provenance_hash: None,
    }
    .sign(&device, &write)
    .unwrap();
    assert!(
        write
            .verifying_key()
            .verify(&dm.core.signing_bytes(), &dm.write_sig)
    );
}

fn dev() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}
fn wt() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
}

/// The wire-presence contract for the fields added within `asset-manifest/v1`
/// (`key_mode`, `wrapped_file_key`, `metadata_blob_hash`): at their defaults they are
/// ABSENT map keys, so manifests signed before the fields existed re-verify
/// byte-identically. See the wire-presence rules in
/// <https://docs/design/cryptography/provenance/>.
#[test]
fn default_new_fields_are_wire_absent() {
    let bytes = core(Action::Create, None).signing_bytes();
    let value: ciborium::Value = cbor::from_slice(&bytes).unwrap();
    let map = value.as_map().expect("manifest core encodes as a CBOR map");
    let keys: Vec<&str> = map.iter().filter_map(|(k, _)| k.as_text()).collect();
    assert!(
        !keys.contains(&"key_mode"),
        "derived key_mode must be wire-absent"
    );
    assert!(!keys.contains(&"wrapped_file_key"));
    assert!(!keys.contains(&"metadata_blob_hash"));
    // The pre-existing options keep their legacy present-null encoding.
    assert!(keys.contains(&"prior_provenance_hash"));
    assert!(keys.contains(&"retention_until"));
}

#[test]
fn wrapped_fields_round_trip_as_byte_strings() {
    let mut c = core(Action::Create, None);
    c.key_mode = KeyMode::Wrapped;
    c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; 60]));
    c.metadata_blob_hash = Some(Hash32([0x4D; 32]));
    let m = c.sign(&dev(), &wt()).unwrap();
    let back: AssetManifest = cbor::from_slice(&cbor::to_canonical_vec(&m).unwrap()).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.signing_bytes(), m.signing_bytes());

    let value: ciborium::Value = cbor::from_slice(&m.signing_bytes()).unwrap();
    let map = value.as_map().unwrap();
    let wrapped = map
        .iter()
        .find(|(k, _)| k.as_text() == Some("wrapped_file_key"))
        .map(|(_, v)| v)
        .expect("wrapped_file_key present when key_mode = wrapped");
    assert!(
        wrapped.is_bytes(),
        "wrapped_file_key must encode as a CBOR byte string"
    );
    // A populated `metadata_blob_hash` (create binds a blob) likewise encodes as a byte
    // string, not an array of integers — the byte-identity vector for the present state.
    let blob_hash = map
        .iter()
        .find(|(k, _)| k.as_text() == Some("metadata_blob_hash"))
        .map(|(_, v)| v)
        .expect("metadata_blob_hash present on a create manifest");
    assert!(
        blob_hash.is_bytes(),
        "metadata_blob_hash must encode as a CBOR byte string"
    );
}
/// An absent lineage is **wire-absent**, and adding the field moved no signature.
///
/// The `dek_public` discipline, applied to `upgraded_from`. A present-`null` inside a
/// signed core changes `signing_bytes` and silently breaks re-verification of every manifest
/// written before the field existed — a failure that shows up as "the signature is wrong"
/// long after the commit that caused it.
#[test]
fn an_absent_lineage_is_wire_absent_and_does_not_move_the_signature() {
    let core = core(Action::Create, None);
    let encoded = core.signing_bytes();
    let decoded: ciborium::Value =
        ciborium::from_reader(encoded.as_slice()).expect("canonical CBOR decodes");
    let ciborium::Value::Map(entries) = decoded else {
        panic!("a manifest core encodes as a map")
    };
    assert!(
        !entries.iter().any(|(key, _)| {
            matches!(key, ciborium::Value::Text(name) if name == "upgraded_from")
        }),
        "the key must be absent, not present-null"
    );

    // And a lineage that *is* present round-trips, so absence is a choice rather than the
    // only thing the encoding can express.
    let mut forked = core;
    forked.upgraded_from = Some(crate::crypto::upgrade::UpgradeLineage {
        old_album_id: Uuid::from_u128(0xA0),
        intent_id: Uuid::from_u128(0x1),
        frozen_state_hash: Hash32([0xEE; 32]),
        from_suite_id: 1,
        to_suite_id: 1,
    });
    let present = forked.signing_bytes();
    assert_ne!(
        present, encoded,
        "a lineage is signed over, so it has to change what the signature covers"
    );
    let back: ManifestCore =
        crate::cbor::from_slice(&present).expect("a core with a lineage decodes");
    assert_eq!(back.upgraded_from, forked.upgraded_from);
}
