use uuid::Uuid;

use super::*;
use crate::cbor;
use crate::crypto::authority::ReferenceAuthority;
use crate::crypto::encryption::{decrypt_asset_vec, seal_file_key, unseal_file_key};
use crate::crypto::hash;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{Amk, AmkVersion, DekKeypair, HybridSigningKey};
use crate::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, WrappedFileKey,
};
use crate::crypto::verify_asset::{VerifyOutcome, verify_asset};

/// Expected KEM-DEM `kem_ct` length under suite `V1`: the X-Wing ciphertext
/// (`DEK_CIPHERTEXT_LEN`) followed by `seal_blob(ss, K)` = suite(2) ‖ nonce(12) ‖ K(32)
/// ‖ tag(16) = 62 bytes.
const EXPECTED_KEM_CT_LEN: usize = DEK_CIPHERTEXT_LEN + 2 + 12 + 32 + 16;

/// Doc "Drop seal round-trip (unit)": seal a plaintext under a random `K` to a Drop Key
/// public half; decapsulate with the private half; STREAM-decrypt; assert byte-equality;
/// assert `kem_ct` length matches the suite; assert the descriptor round-trips through
/// canonical CBOR.
#[test]
fn drop_seal_round_trip() {
    let drop_key = DekKeypair::generate();
    let plaintext = b"a guest's photo bytes, sealed under a fresh random K in the browser";

    let sealed = seal_drop(plaintext, &drop_key.public_bytes(), "image/jpeg").unwrap();

    // The encapsulated-key length is fixed by the suite (server-observable, deterministic).
    assert_eq!(sealed.descriptor.kem_ct.len(), EXPECTED_KEM_CT_LEN);
    assert_eq!(sealed.descriptor.plaintext_size, plaintext.len() as u64);
    // The descriptor commits to the exact ciphertext bytes.
    assert_eq!(
        sealed.descriptor.ciphertext_hash,
        hash::hash_bytes(&sealed.ciphertext)
    );

    // The Drop Key holder recovers K, and STREAM-decrypt returns the exact plaintext.
    let k = open_drop_key(&drop_key, &sealed.descriptor.kem_ct).unwrap();
    let back = decrypt_asset_vec(&k, &sealed.descriptor.nonce_prefix, &sealed.ciphertext).unwrap();
    assert_eq!(back, plaintext);

    // The descriptor round-trips byte-identically through canonical CBOR.
    let bytes = cbor::to_canonical_vec(&sealed.descriptor).unwrap();
    let decoded: DropDescriptor = cbor::from_slice(&bytes).unwrap();
    assert_eq!(decoded, sealed.descriptor);

    // A different Drop Key cannot recover K (server-blindness / contribute-only).
    let attacker = DekKeypair::generate();
    let wrong = open_drop_key(&attacker, &sealed.descriptor.kem_ct);
    // Decapsulation yields a different shared secret → the AEAD wrap open fails closed.
    assert!(wrong.is_err() || wrong.unwrap() != k);
}

/// A minimal, fully-valid signing setup so the adoption test can build a manifest
/// `verify_asset` accepts — the same construction the provenance suite's `Fixture` uses.
struct AdopterFixture {
    device: HybridSigningKey,
    write: HybridSigningKey,
    directory: crate::crypto::keys::DeviceDirectory,
    authority: ReferenceAuthority,
    user_id: Uuid,
    device_id: Uuid,
    album_id: Uuid,
}

impl AdopterFixture {
    fn new() -> Self {
        let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
        let user_id = Uuid::from_u128(0x05E2);
        let device_id = Uuid::from_u128(0xD1);
        let album_id = Uuid::from_u128(0xA1);

        let directory = DirectoryCore {
            user_id,
            directory_version: 1,
            updated_at: "2026-05-30T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id,
                dsk_public: device.verifying_key(),
                dek_public: None,
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
                unknown: Default::default(),
            }],
            unknown: Default::default(),
        }
        .sign(&ik);

        let authority = ReferenceAuthority::new(album_id, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &write.verifying_key(),
            true,
        );

        Self {
            device,
            write,
            directory,
            authority,
            user_id,
            device_id,
            album_id,
        }
    }
}

/// Doc "Adoption rewrap accepts (unit)": decapsulate a drop, rewrap `K` under a test AMK
/// (`asset-keywrap/v1`), build the `create` manifest with `key_mode = wrapped`; assert
/// `verify_asset` accepts and a second member can unwrap `wrapped_file_key` and
/// STREAM-decrypt the unchanged ciphertext.
#[test]
fn adoption_rewrap_verifies_and_decrypts() {
    let f = AdopterFixture::new();
    let amk = Amk::from_bytes([0x5A; 32]);
    let file_id = Uuid::from_u128(0xF11E);

    // A guest seals a drop to the link's Drop Key (the browser/WASM half).
    let drop_key = DekKeypair::generate();
    let plaintext = b"adopted web-upload drop bytes, chosen by an external party";
    let sealed = seal_drop(plaintext, &drop_key.public_bytes(), "image/jpeg").unwrap();

    // The adopter decapsulates K and rewraps it under the destination album's AMK.
    let k = open_drop_key(&drop_key, &sealed.descriptor.kem_ct).unwrap();
    let wrapped_file_key = seal_file_key(&amk, &file_id, &k);

    // Build + sign the adopting `create` manifest — key_mode = wrapped, ciphertext_hash
    // and nonce_prefix carried from the drop; the adopter is the cryptographic author.
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id,
        album_id: f.album_id,
        amk_version: AmkVersion(1),
        ciphertext_hash: sealed.descriptor.ciphertext_hash,
        plaintext_size: sealed.descriptor.plaintext_size,
        chunk_size: sealed.descriptor.chunk_size,
        nonce_prefix: sealed.descriptor.nonce_prefix,
        key_mode: KeyMode::Wrapped,
        wrapped_file_key: Some(WrappedFileKey(wrapped_file_key)),
        metadata_blob_hash: Some(hash::hash_bytes(b"the adopter's freshly authored sidecar")),
        created_by_user: f.user_id,
        created_by_device: f.device_id,
        client_version: "capsule-core/0.1.0".into(),
        timestamp: "2026-05-31T12:00:00Z".into(),
        action: Action::Create,
        prior_provenance_hash: None,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: None,
    };
    let manifest = core.sign(&f.device, &f.write).unwrap();

    // verify_asset accepts the adopting manifest against the unchanged drop ciphertext.
    assert_eq!(
        verify_asset(
            &manifest,
            &sealed.ciphertext,
            &f.directory,
            &f.authority,
            None
        ),
        VerifyOutcome::Accept,
    );

    // A second album member (holding the same AMK) unwraps K and decrypts the bytes.
    let recovered = unseal_file_key(
        &amk,
        &file_id,
        &manifest.core.wrapped_file_key.as_ref().unwrap().0,
    )
    .unwrap();
    assert_eq!(recovered, k);
    assert_eq!(
        decrypt_asset_vec(
            &recovered,
            &sealed.descriptor.nonce_prefix,
            &sealed.ciphertext
        )
        .unwrap(),
        plaintext,
    );
}

/// `seal_drop_derand` is a pure function of its inputs (byte-reproducible), and its output is
/// adoptable exactly like a CSPRNG-sealed drop — the property the cross-language KAT and
/// its Rust adopter rely on (the browser reproduces these same bytes; the Rust side adopts
/// them). Also asserts the derandomized seal agrees with the CSPRNG path's *shape*.
#[test]
fn seal_drop_derand_is_reproducible_and_adoptable() {
    let drop_key = DekKeypair::from_seed(&[0x5D; 32]);
    let pk = drop_key.public_bytes();
    let plaintext = b"a deterministic guest drop, sealed under fixed key material";
    let k = [0x11; 32];
    let nonce_prefix = [0x22; NONCE_PREFIX_LEN];
    let eseed = [0x33; ESEED_LEN];
    let blob_nonce = [0x44; 12];

    let a = seal_drop_derand(
        plaintext,
        &pk,
        "image/png",
        &k,
        &nonce_prefix,
        &eseed,
        &blob_nonce,
    )
    .unwrap();
    let b = seal_drop_derand(
        plaintext,
        &pk,
        "image/png",
        &k,
        &nonce_prefix,
        &eseed,
        &blob_nonce,
    )
    .unwrap();

    // Byte-for-byte reproducible: identical descriptor and ciphertext.
    assert_eq!(a.descriptor, b.descriptor);
    assert_eq!(a.ciphertext, b.ciphertext);
    assert_eq!(a.descriptor.nonce_prefix, nonce_prefix);
    assert_eq!(a.descriptor.kem_ct.len(), EXPECTED_KEM_CT_LEN);

    // The Drop Key holder recovers K and STREAM-decrypt returns the exact plaintext — the
    // deterministic seal is adoptable just like the CSPRNG path.
    let recovered = open_drop_key(&drop_key, &a.descriptor.kem_ct).unwrap();
    assert_eq!(recovered, k);
    let back = decrypt_asset_vec(&recovered, &a.descriptor.nonce_prefix, &a.ciphertext).unwrap();
    assert_eq!(back, plaintext);
}

/// Doc "Opaque-id entropy (unit)": generated upload-link ids are ≥128-bit CSPRNG values
/// and non-sequential — never UUIDv7 or otherwise structured (identical to the
/// share-link check).
#[test]
fn opaque_id_entropy() {
    assert_eq!(OPAQUE_ID_LEN, 16, "opaque id must be a full 128 bits");
    assert_eq!(generate_opaque_id().len(), 16);

    let ids: Vec<[u8; OPAQUE_ID_LEN]> = (0..256).map(|_| generate_opaque_id()).collect();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(ids[i], ids[j], "two CSPRNG opaque ids must never collide");
        }
    }
    // No UUIDv7 version/variant structure, and not time-ordered like a v7 timestamp.
    assert!(
        ids.iter().any(|id| (id[6] >> 4) != 0x7),
        "opaque ids must not all carry the UUIDv7 version nibble"
    );
    assert!(
        ids.iter().any(|id| (id[8] & 0xc0) != 0x80),
        "opaque ids must not all carry the UUID variant bits"
    );
    let high48 = |id: &[u8; OPAQUE_ID_LEN]| {
        id[..6]
            .iter()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
    };
    assert!(
        ids.windows(2).any(|w| high48(&w[0]) >= high48(&w[1])),
        "opaque ids must not be time-ordered/sequential like a UUIDv7"
    );
}

/// Doc "No library injection (unit)" — the client-side half: a `DropDescriptor` carries
/// **no** authorization or album-binding fields (no signature, no `album_id`, no
/// `amk_version`, no provenance link), so it cannot be presented on an album-write path.
/// Its CBOR is disjoint from an `AssetManifest`'s.
#[test]
fn drop_descriptor_carries_no_authorization_fields() {
    let drop_key = DekKeypair::generate();
    let sealed = seal_drop(b"bytes", &drop_key.public_bytes(), "image/jpeg").unwrap();
    let bytes = cbor::to_canonical_vec(&sealed.descriptor).unwrap();

    // The descriptor never decodes as an AssetManifest: it lacks device_sig/write_sig,
    // album_id, amk_version, and every provenance field a signed write must carry.
    assert!(
        cbor::from_slice::<AssetManifest>(&bytes).is_err(),
        "a DropDescriptor must not be interpretable as a signed AssetManifest"
    );
}

/// The passphrase abuse-gate verifier round-trips: the correct passphrase verifies, a
/// wrong one does not, and the passphrase never appears in the stored verifier bytes.
#[test]
fn passphrase_verifier_gates_without_leaking() {
    const PW: &str = "spend my quota if you can";
    let fast = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let v = PassphraseVerifier::derive(PW, fast).unwrap();
    assert!(v.verify(PW));
    assert!(!v.verify("wrong"));

    // The passphrase is nowhere in the stored salt or verifier.
    let pw = PW.as_bytes();
    let contains = |h: &[u8]| h.windows(pw.len()).any(|w| w == pw);
    assert!(!contains(&v.salt));
    assert!(!contains(&v.verifier));
}
