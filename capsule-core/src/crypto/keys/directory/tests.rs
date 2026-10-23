use std::sync::Arc;

use super::*;
use crate::crypto::keys::kem::DekKeypair;
use crate::crypto::keys::kem_p256::{MockP256KeyAgreement, P256HybridDek};

fn entry(device: &HybridSigningKey, dek_public: Option<DekPublic>) -> DeviceEntry {
    DeviceEntry {
        device_id: Uuid::from_u128(0xD1),
        dsk_public: device.verifying_key(),
        dek_public,
        added_at: "2026-05-30T00:00:00Z".into(),
        revoked_at: None,
        unknown: Default::default(),
    }
}

fn dir(version: u64, ik: &HybridSigningKey, device: &HybridSigningKey) -> DeviceDirectory {
    core(version, device).sign(ik)
}

fn core(version: u64, device: &HybridSigningKey) -> DirectoryCore {
    DirectoryCore {
        user_id: Uuid::from_u128(1),
        directory_version: version,
        updated_at: "2026-05-31T00:00:00Z".into(),
        devices: vec![entry(device, None)],
        unknown: Default::default(),
    }
}

fn ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}
fn dev() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
}

/// The CBOR map for the sole device entry inside a core's canonical signing bytes.
fn entry_keys(core: &DirectoryCore) -> Vec<String> {
    let value: ciborium::Value = crate::cbor::from_slice(&core.signing_bytes()).unwrap();
    entry_map(&value)
        .iter()
        .filter_map(|(k, _)| k.as_text().map(str::to_owned))
        .collect()
}

fn entry_map(value: &ciborium::Value) -> Vec<(ciborium::Value, ciborium::Value)> {
    let map = value
        .as_map()
        .expect("directory core encodes as a CBOR map");
    let devices = map
        .iter()
        .find(|(k, _)| k.as_text() == Some("devices"))
        .map(|(_, v)| v)
        .expect("devices key");
    devices.as_array().expect("devices is an array")[0]
        .as_map()
        .expect("a device entry encodes as a CBOR map")
        .clone()
}

#[test]
fn sign_verify_and_lookup() {
    let (ik, dev) = (ik(), dev());
    let d = dir(1, &ik, &dev);

    assert!(d.verify(&ik.verifying_key()));
    // Wrong IK does not verify.
    assert!(!d.verify(&HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]).verifying_key()));
    // Lookup.
    assert_eq!(
        d.device(&Uuid::from_u128(0xD1)).unwrap().dsk_public,
        dev.verifying_key()
    );
    assert!(d.device(&Uuid::from_u128(0xDEAD)).is_none());
}

#[test]
fn tampering_with_a_device_key_breaks_the_signature() {
    let (ik, dev) = (ik(), dev());
    let mut d = dir(1, &ik, &dev);
    // Swap in a different device key without re-signing.
    d.core.devices[0].dsk_public =
        HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]).verifying_key();
    assert!(!d.verify(&ik.verifying_key()));
}

#[test]
fn serializes_canonically() {
    let (ik, dev) = (ik(), dev());
    let d = dir(7, &ik, &dev);
    let bytes = crate::cbor::to_canonical_vec(&d).unwrap();
    let back: DeviceDirectory = crate::cbor::from_slice(&bytes).unwrap();
    assert_eq!(back, d);
    assert!(back.verify(&ik.verifying_key()));
}

// ── Cross-signing a new device in ───────────────────────────────────────────

fn second() -> DeviceEntry {
    DeviceEntry {
        device_id: Uuid::from_u128(0xD2),
        dsk_public: HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]).verifying_key(),
        dek_public: Some(DekPublic(DekKeypair::generate().public_bytes())),
        added_at: "2026-06-01T00:00:00Z".into(),
        revoked_at: None,
        unknown: Default::default(),
    }
}

/// The new device is appended at the next version, the existing one survives, and the
/// re-signed directory verifies and lets a peer encapsulate to the new device.
#[test]
fn adding_a_device_bumps_the_version_and_keeps_the_others() {
    let (ik, dev) = (ik(), dev());
    let mut c = core(3, &dev);
    let new = second();
    c.add_device(new.clone(), "2026-06-01T00:00:01Z".into())
        .unwrap();
    assert_eq!(c.directory_version, 4);
    assert_eq!(c.updated_at, "2026-06-01T00:00:01Z");
    let d = c.sign(&ik);
    assert!(d.verify(&ik.verifying_key()));
    assert!(d.device(&Uuid::from_u128(0xD1)).is_some());
    assert_eq!(d.device(&Uuid::from_u128(0xD2)), Some(&new));
    assert!(
        d.device(&Uuid::from_u128(0xD2))
            .unwrap()
            .encapsulate()
            .is_ok()
    );
}

/// Every refusal leaves the core exactly as it was — no half-applied version bump.
#[test]
fn a_refused_add_leaves_the_core_unchanged() {
    let dev = dev();
    let original = core(3, &dev);

    let mut duplicate_id = second();
    duplicate_id.device_id = Uuid::from_u128(0xD1);
    let mut duplicate_key = second();
    duplicate_key.dsk_public = dev.verifying_key();
    let mut revoked = second();
    revoked.revoked_at = Some("2026-06-02T00:00:00Z".into());

    let cases = [
        (
            duplicate_id,
            DirectoryError::DeviceAlreadyListed(Uuid::from_u128(0xD1)),
        ),
        (
            duplicate_key,
            DirectoryError::SigningKeyAlreadyListed {
                new: Uuid::from_u128(0xD2),
                existing: Uuid::from_u128(0xD1),
            },
        ),
        (revoked, DirectoryError::EntryRevoked(Uuid::from_u128(0xD2))),
    ];
    for (entry, expected) in cases {
        let mut c = original.clone();
        assert_eq!(c.add_device(entry, "later".into()), Err(expected));
        assert_eq!(c, original);
    }

    let mut c = original.clone();
    c.directory_version = u64::MAX;
    let before = c.clone();
    assert_eq!(
        c.add_device(second(), "later".into()),
        Err(DirectoryError::VersionExhausted(u64::MAX))
    );
    assert_eq!(c, before);
}

/// A revoked entry still holds its id: a revoked device cannot be re-enrolled under it.
#[test]
fn a_revoked_entry_still_reserves_its_id() {
    let mut c = core(1, &dev());
    c.devices[0].revoked_at = Some("2026-06-02T00:00:00Z".into());
    let mut again = second();
    again.device_id = Uuid::from_u128(0xD1);
    assert_eq!(
        c.add_device(again, "later".into()),
        Err(DirectoryError::DeviceAlreadyListed(Uuid::from_u128(0xD1)))
    );
}

// ── `dek_public` wire presence ──────────────────────────────

/// The wire-presence contract for `dek_public`: with no DEK published it is an **ABSENT**
/// map key, so directories signed before the field existed re-verify byte-identically.
/// Mirrors `manifest.rs`'s `default_new_fields_are_wire_absent`.
#[test]
fn default_dek_public_is_wire_absent() {
    let keys = entry_keys(&core(1, &dev()));
    assert!(
        !keys.iter().any(|k| k == "dek_public"),
        "an unpublished dek_public must be a wire-absent key, not a present null: {keys:?}"
    );
    // The pre-existing options keep their legacy present-null encoding — reconciling them
    // would itself be a signature-visible change to entries already signed.
    assert!(keys.iter().any(|k| k == "added_at"));
    assert!(keys.iter().any(|k| k == "revoked_at"));
}

/// **Directories signed before `dek_public` must keep verifying.** `LegacyDirectoryCore` is
/// the encoder from before `dek_public` existed, verbatim — the struct that produced every
/// directory signed before then. Bytes it produced and the IK signed must (a) decode under
/// today's type, (b) re-derive **byte-identical** signing bytes, and (c) still verify.
#[test]
fn a_directory_signed_before_dek_public_existed_still_verifies() {
    #[derive(Serialize)]
    struct LegacyDeviceEntry {
        device_id: Uuid,
        dsk_public: HybridVerifyingKey,
        added_at: String,
        revoked_at: Option<String>,
    }
    #[derive(Serialize)]
    struct LegacyDirectoryCore {
        user_id: Uuid,
        directory_version: u64,
        updated_at: String,
        devices: Vec<LegacyDeviceEntry>,
    }

    let (ik, dev) = (ik(), dev());
    let legacy = LegacyDirectoryCore {
        user_id: Uuid::from_u128(1),
        directory_version: 7,
        updated_at: "2026-05-31T00:00:00Z".into(),
        devices: vec![LegacyDeviceEntry {
            device_id: Uuid::from_u128(0xD1),
            dsk_public: dev.verifying_key(),
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
        }],
    };
    let legacy_bytes = crate::cbor::to_canonical_vec(&legacy).unwrap();
    let legacy_signature = ik.sign(&legacy_bytes);

    // Decode the pre-change bytes with today's type…
    let decoded: DirectoryCore = crate::cbor::from_slice(&legacy_bytes).unwrap();
    assert_eq!(
        decoded.devices[0].dek_public, None,
        "an absent key must decode as None, not fail"
    );
    // …and re-encoding must reproduce the signed bytes exactly.
    assert_eq!(
        decoded.signing_bytes(),
        legacy_bytes,
        "adding dek_public changed the signing bytes of a pre-existing entry"
    );
    // Which is what makes the pre-change signature still verify.
    let directory = DeviceDirectory {
        core: decoded,
        signature: legacy_signature,
    };
    assert!(
        directory.verify(&ik.verifying_key()),
        "a directory signed before dek_public existed must still verify"
    );
    // And today's encoder produces those same bytes for the same logical entry.
    assert_eq!(core(7, &dev).signing_bytes(), legacy_bytes);
}

/// A published DEK is a CBOR **byte string**, is present on the wire, and is covered by the
/// IK signature — tampering with it invalidates the directory.
#[test]
fn a_published_dek_is_a_signed_byte_string() {
    let (ik, dev) = (ik(), dev());
    let dek = DekKeypair::generate();
    let mut c = core(1, &dev);
    c.devices[0].dek_public = Some(DekPublic(dek.public_bytes()));

    let keys = entry_keys(&c);
    assert!(
        keys.iter().any(|k| k == "dek_public"),
        "a published dek_public must be present on the wire: {keys:?}"
    );
    let value: ciborium::Value = crate::cbor::from_slice(&c.signing_bytes()).unwrap();
    let published = entry_map(&value)
        .into_iter()
        .find(|(k, _)| k.as_text() == Some("dek_public"))
        .map(|(_, v)| v)
        .expect("dek_public present");
    assert!(
        published.is_bytes(),
        "dek_public must encode as a CBOR byte string, not an array of integers"
    );

    let mut d = c.sign(&ik);
    assert!(d.verify(&ik.verifying_key()));

    // Round-trips through canonical CBOR unchanged.
    let back: DeviceDirectory =
        crate::cbor::from_slice(&crate::cbor::to_canonical_vec(&d).unwrap()).unwrap();
    assert_eq!(back, d);
    assert!(back.verify(&ik.verifying_key()));

    // The signature covers it: swapping in another device's DEK breaks verification.
    d.core.devices[0].dek_public = Some(DekPublic(DekKeypair::generate().public_bytes()));
    assert!(
        !d.verify(&ik.verifying_key()),
        "a substituted dek_public must break the IK signature"
    );
}

// ── Unknown keys from a newer writer (versioning invariant 2) ───────────────

/// A value no build has defined: a tag, a full-width integer and a nested map with a float.
fn future_value() -> ciborium::Value {
    use ciborium::Value;
    Value::Map(vec![
        (
            Value::Text("captured".into()),
            Value::Tag(1, Box::new(Value::from(1_790_000_000u64))),
        ),
        (
            Value::Text("weights".into()),
            Value::Array(vec![Value::Float(0.5), Value::from(u64::MAX)]),
        ),
    ])
}

/// Splice `extra` into the CBOR map `value` (which must be one).
fn with_keys(value: ciborium::Value, extra: &[(&str, ciborium::Value)]) -> ciborium::Value {
    let ciborium::Value::Map(mut entries) = value else {
        panic!("a CBOR map")
    };
    for (k, v) in extra {
        entries.push((ciborium::Value::Text((*k).to_owned()), v.clone()));
    }
    ciborium::Value::Map(entries)
}

/// A "future" writer that does not use these types: it adds keys this build has never
/// heard of to the directory core and to a device entry, and signs the canonical core it
/// built. This build decodes it, keeps every key, re-emits the core byte for byte, and so
/// verifies; dropping either set of keys fails verification.
#[test]
fn a_directory_from_a_newer_writer_keeps_its_unknown_keys_under_the_signature() {
    let (ik, dev) = (ik(), dev());
    let current: ciborium::Value = crate::cbor::from_slice(&core(4, &dev).signing_bytes()).unwrap();
    let ciborium::Value::Map(entries) = current else {
        panic!("a core is a map")
    };
    let entries = entries
        .into_iter()
        .map(|(k, v)| {
            if k.as_text() == Some("devices") {
                let devices = v
                    .into_array()
                    .unwrap()
                    .into_iter()
                    .map(|d| with_keys(d, &[("x_attested_by", future_value())]))
                    .collect();
                (k, ciborium::Value::Array(devices))
            } else {
                (k, v)
            }
        })
        .collect();
    let future_core = with_keys(
        ciborium::Value::Map(entries),
        &[
            ("x_recovery_policy", future_value()),
            ("z", ciborium::Value::Bytes(vec![0xFF; 4])),
        ],
    );
    let signed_bytes = crate::cbor::value_to_canonical_vec(&future_core);
    let wire = crate::cbor::value_to_canonical_vec(&ciborium::Value::Map(vec![
        (ciborium::Value::Text("core".into()), future_core.clone()),
        (
            ciborium::Value::Text("signature".into()),
            crate::cbor::from_slice(
                &crate::cbor::to_canonical_vec(&ik.sign(&signed_bytes)).unwrap(),
            )
            .unwrap(),
        ),
    ]));

    let directory: DeviceDirectory = crate::cbor::from_slice(&wire).unwrap();
    assert_eq!(
        directory.core.unknown.keys().collect::<Vec<_>>(),
        ["x_recovery_policy", "z"]
    );
    assert_eq!(
        directory.core.devices[0].unknown.keys().collect::<Vec<_>>(),
        ["x_attested_by"]
    );
    assert_eq!(directory.core.signing_bytes(), signed_bytes);
    assert!(directory.verify(&ik.verifying_key()));
    assert_eq!(crate::cbor::to_canonical_vec(&directory).unwrap(), wire);

    let mut stripped_core = directory.clone();
    stripped_core.core.unknown.clear();
    assert!(
        !stripped_core.verify(&ik.verifying_key()),
        "dropping a core key must fail verification"
    );
    let mut stripped_entry = directory;
    stripped_entry.core.devices[0].unknown.clear();
    assert!(
        !stripped_entry.verify(&ik.verifying_key()),
        "dropping a device-entry key must fail verification"
    );
}

/// Everything this build writes has empty unknown maps, and an empty map is wire-absent: the
/// core encodes exactly the four keys it always had.
#[test]
fn an_empty_unknown_map_is_wire_absent() {
    let c = core(1, &dev());
    let value: ciborium::Value = crate::cbor::from_slice(&c.signing_bytes()).unwrap();
    let keys: Vec<_> = value
        .as_map()
        .unwrap()
        .iter()
        .filter_map(|(k, _)| k.as_text())
        .collect();
    assert_eq!(
        keys,
        ["devices", "user_id", "updated_at", "directory_version"]
    );
    assert_eq!(
        entry_keys(&c),
        ["added_at", "device_id", "dsk_public", "revoked_at"]
    );
}

// ── Encapsulating to a published entry (the point of publishing it) ─────────

/// **The slice's acceptance.** A peer holding only the published entry wraps a secret to the
/// device, and the device — and only the device — recovers it.
#[test]
fn a_peer_encapsulates_to_a_published_software_dek() {
    let (ik, dev) = (ik(), dev());
    let dek = DekKeypair::generate();
    let mut c = core(1, &dev);
    c.devices[0].dek_public = Some(DekPublic(dek.public_bytes()));
    let directory = c.sign(&ik);
    assert!(directory.verify(&ik.verifying_key()));

    let published = directory.device(&Uuid::from_u128(0xD1)).unwrap();
    let (ciphertext, sent) = published.encapsulate().unwrap();
    assert_eq!(
        dek.decapsulate(&ciphertext).unwrap(),
        sent,
        "the device must recover the secret a peer wrapped to its published entry"
    );
    // A different device's DEK does not recover it.
    assert_ne!(
        DekKeypair::generate().decapsulate(&ciphertext).ok(),
        Some(sent)
    );
}

/// The hardware-bound composition rides the same field: the entry carries `ek_M ‖ pk_P` and
/// the length alone tells a peer which KEM to run — no tag, no out-of-band hint.
#[test]
fn a_peer_encapsulates_to_a_published_hardware_dek() {
    let element = Arc::new(MockP256KeyAgreement::new([7u8; 32], false));
    let dek = P256HybridDek::enroll(element, "device-dek".into(), &[3u8; 32]).unwrap();
    let published = entry(&dev(), Some(DekPublic(dek.public_bytes())));

    assert_eq!(
        published.dek_public.as_ref().unwrap().as_bytes().len(),
        DEK_P256_PUBLIC_LEN,
        "the P-256 hybrid half is length-disjoint from X-Wing"
    );
    let (ciphertext, sent) = published.encapsulate().unwrap();
    assert_eq!(dek.decapsulate(&ciphertext).unwrap(), sent);
}

/// The two compositions never alias: a peer that runs the wrong KEM produces a ciphertext the
/// device rejects, so length dispatch is what makes a single field sound.
#[test]
fn the_two_compositions_are_length_disjoint() {
    let software = DekKeypair::generate();
    let element = Arc::new(MockP256KeyAgreement::new([9u8; 32], false));
    let hardware = P256HybridDek::enroll(element, "device-dek".into(), &[5u8; 32]).unwrap();

    assert_eq!(software.public_bytes().len(), DEK_PUBLIC_LEN);
    assert_eq!(hardware.public_bytes().len(), DEK_P256_PUBLIC_LEN);
    assert_ne!(DEK_PUBLIC_LEN, DEK_P256_PUBLIC_LEN);

    // The ciphertext a peer produces for one is refused by the other.
    let (hardware_ct, _) = entry(&dev(), Some(DekPublic(hardware.public_bytes())))
        .encapsulate()
        .unwrap();
    assert!(software.decapsulate(&hardware_ct).is_err());
}

#[test]
fn encapsulating_to_a_device_with_no_published_dek_is_refused() {
    assert!(matches!(
        entry(&dev(), None).encapsulate(),
        Err(CryptoError::Key("device publishes no DEK"))
    ));
}

/// A revoked device must not receive new key wraps, even though its entry is retained so its
/// past manifests stay verifiable.
#[test]
fn encapsulating_to_a_revoked_device_is_refused() {
    let mut e = entry(
        &dev(),
        Some(DekPublic(DekKeypair::generate().public_bytes())),
    );
    e.revoked_at = Some("2026-06-01T00:00:00Z".into());
    assert!(matches!(
        e.encapsulate(),
        Err(CryptoError::Key("device is revoked"))
    ));
}

/// A published key of an unknown length is refused rather than guessed at — the failure mode
/// a tag would otherwise have to be trusted to prevent.
#[test]
fn a_dek_of_unknown_length_is_refused() {
    for bytes in [vec![], vec![0u8; 32], vec![0u8; DEK_PUBLIC_LEN - 1]] {
        assert!(matches!(
            entry(&dev(), Some(DekPublic(bytes))).encapsulate(),
            Err(CryptoError::Key(
                "published DEK length matches no known composition"
            ))
        ));
    }
}

/// A well-formed length with garbage bytes fails in the KEM, not with a wrong secret.
#[test]
fn a_malformed_dek_of_the_right_length_fails_in_the_kem() {
    assert!(
        entry(&dev(), Some(DekPublic(vec![0xFF; DEK_P256_PUBLIC_LEN])))
            .encapsulate()
            .is_err()
    );
}
