//! Guest-drop adoption Known-Answer Test (slice `S-D3`) — the **Rust adoption half** of the
//! cross-language KAT whose browser half lives in `capsule-web`'s `drop-seal.test.ts`.
//!
//! The browser seals a guest drop; the album owner's native (trusted) client adopts it. This test
//! is the adoption side, keyed on the **same fixed inputs** as the fixture generator
//! (`xtask/src/drop_kat.rs`), so the bytes proven byte-identical to the browser seal in the bun KAT
//! are exactly the bytes adopted here. It walks the real adoption path end to end:
//!
//!   decapsulate `K` → rewrap under the album AMK (`asset-keywrap/v1`) → sign a `create` manifest
//!   with `key_mode = wrapped` → `verify_asset` **Accept** → a second album member unwraps
//!   `wrapped_file_key` and STREAM-decrypts the unchanged drop ciphertext back to the plaintext.
//!
//! This is E2E case 13's browser→library path at the level the repo runs locally; a live-browser
//! run is still owed (as for S-E1/S-D6). The fixed inputs below mirror `xtask/src/drop_kat.rs`.

use capsule_core::crypto::authority::ReferenceAuthority;
use capsule_core::crypto::encryption::keywrap::{seal_file_key, unseal_file_key};
use capsule_core::crypto::encryption::stream::decrypt_asset_vec;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use capsule_core::crypto::keys::{Amk, AmkVersion, DekKeypair, DeviceDirectory, HybridSigningKey};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::{
    ASSET_MANIFEST_VERSION, KeyMode, ManifestCore, WrappedFileKey,
};
use capsule_core::crypto::verify_asset::{VerifyOutcome, verify_asset};
use capsule_core::drop::{open_drop_key, seal_drop_derand};
use uuid::Uuid;

/// A minimal, fully-valid adopter signing setup — the same construction the provenance suite and
/// the `capsule-core::drop` unit test use, so `verify_asset` has a directory + authority to accept
/// against.
struct AdopterFixture {
    device: HybridSigningKey,
    write: HybridSigningKey,
    directory: DeviceDirectory,
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
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
            }],
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

/// The browser-reproduced drop (`seal_drop_derand` with the fixture's fixed inputs) is adopted:
/// decapsulate, rewrap under the album AMK, sign a `create`, and assert `verify_asset` accepts and
/// a second member can recover the exact plaintext.
#[test]
fn browser_sealed_drop_adopts_and_verifies() {
    // Fixed inputs — mirrored in xtask/src/drop_kat.rs. The bun KAT proves the WASM seal produces
    // byte-identical output for these; here that same output is adopted.
    let drop_seed = [0x5D; 32];
    let k = [0x11; 32];
    let nonce_prefix = [0x22; 7];
    let eseed = [0x33; 64];
    let blob_nonce = [0x44; 12];
    let plaintext: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();

    // The guest's browser sealed this to the link's Drop Key public half.
    let drop_key = DekKeypair::from_seed(&drop_seed);
    let sealed = seal_drop_derand(
        &plaintext,
        &drop_key.public_bytes(),
        "image/jpeg",
        &k,
        &nonce_prefix,
        &eseed,
        &blob_nonce,
    )
    .expect("seal the deterministic guest drop");

    // The adopter decapsulates K with the Drop Key private half and rewraps it under the album AMK.
    let recovered_k = open_drop_key(&drop_key, &sealed.descriptor.kem_ct).expect("decapsulate K");
    assert_eq!(recovered_k, k, "the adopter recovers the guest's chosen K");

    let f = AdopterFixture::new();
    let amk = Amk::from_bytes([0x5A; 32]);
    let file_id = Uuid::from_u128(0xF11E);
    let wrapped_file_key = seal_file_key(&amk, &file_id, &recovered_k);

    // Build + sign the adopting `create` manifest (key_mode = wrapped), carrying the drop's
    // ciphertext hash and nonce prefix; the adopter is the cryptographic author.
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
        metadata_blob_hash: Some(hash_bytes(b"the adopter's freshly authored sidecar")),
        created_by_user: f.user_id,
        created_by_device: f.device_id,
        client_version: "capsule-core/0.1.0".into(),
        timestamp: "2026-05-31T12:00:00Z".into(),
        action: Action::Create,
        prior_provenance_hash: None,
        retention_until: None,
    };
    let manifest = core
        .sign(&f.device, &f.write)
        .expect("sign the adopting manifest");

    // verify_asset accepts the adopting manifest against the unchanged browser drop ciphertext.
    assert_eq!(
        verify_asset(
            &manifest,
            &sealed.ciphertext,
            &f.directory,
            &f.authority,
            None
        ),
        VerifyOutcome::Accept,
        "the adopted browser-sealed drop verifies",
    );

    // A second album member (holding the same AMK) unwraps K and decrypts the exact plaintext.
    let member_k = unseal_file_key(
        &amk,
        &file_id,
        &manifest.core.wrapped_file_key.as_ref().unwrap().0,
    )
    .expect("a member unwraps the file key");
    assert_eq!(member_k, k);
    assert_eq!(
        decrypt_asset_vec(
            &member_k,
            &sealed.descriptor.nonce_prefix,
            &sealed.ciphertext
        )
        .expect("STREAM-decrypt the drop ciphertext"),
        plaintext,
        "the recovered plaintext is byte-identical to the guest's original",
    );
}
