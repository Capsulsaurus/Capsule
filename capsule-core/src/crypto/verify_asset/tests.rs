use uuid::Uuid;

use super::*;
use crate::crypto::authority::ReferenceAuthority;
use crate::crypto::encryption::{
    decrypt_asset_vec, encrypt_asset_vec_with_prefix, seal_file_key, unseal_file_key,
};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, WrappedFileKey,
};

const USER: u128 = 0x05E2;
const DEVICE: u128 = 0xD1;
const ALBUM: u128 = 0xA1;
const CIPHERTEXT: &[u8] = b"the asset ciphertext bytes";

/// A fully-valid setup; tests perturb exactly one thing.
struct Fixture {
    device: HybridSigningKey,
    write1: HybridSigningKey,
    write2: HybridSigningKey,
    admin: HybridSigningKey,
    directory: DeviceDirectory,
    authority: ReferenceAuthority,
}

impl Fixture {
    fn new() -> Self {
        let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write1 = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let write2 = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]);
        let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);

        let directory = DirectoryCore {
            user_id: Uuid::from_u128(USER),
            directory_version: 1,
            updated_at: "2026-05-30T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: Uuid::from_u128(DEVICE),
                dsk_public: device.verifying_key(),
                dek_public: None,
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
                unknown: Default::default(),
            }],
            unknown: Default::default(),
        }
        .sign(&ik);

        // Authority attests epoch 1 (AMK present) and epoch 2 (AMK present).
        let authority = ReferenceAuthority::new(Uuid::from_u128(ALBUM), admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &write1.verifying_key(), true)
            .with_epoch(&admin, AmkVersion(2), &write2.verifying_key(), true);

        Self {
            device,
            write1,
            write2,
            admin,
            directory,
            authority,
        }
    }

    fn core(&self, action: Action, prior: Option<Hash32>) -> ManifestCore {
        ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: Uuid::from_u128(0xF11E),
            album_id: Uuid::from_u128(ALBUM),
            amk_version: AmkVersion(1),
            ciphertext_hash: hash::hash_bytes(CIPHERTEXT),
            plaintext_size: 12,
            chunk_size: 65_520,
            nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            // Present exactly on the metadata-bearing actions, so `structural_ok` (checked
            // before any signature) holds for every otherwise-valid fixture.
            metadata_blob_hash: action.binds_metadata_blob().then_some(Hash32([0x4D; 32])),
            created_by_user: Uuid::from_u128(USER),
            created_by_device: Uuid::from_u128(DEVICE),
            client_version: "capsule-cli/0.1.0".into(),
            timestamp: "2026-05-31T12:00:00Z".into(),
            action,
            prior_provenance_hash: prior,
            upgraded_from: None,
            unknown: Default::default(),
            retention_until: None,
        }
    }

    /// A valid `create` manifest signed by the correct device + epoch-1 write key.
    fn valid_create(&self) -> AssetManifest {
        self.core(Action::Create, None)
            .sign(&self.device, &self.write1)
            .unwrap()
    }

    fn verify(&self, m: &AssetManifest, head: Option<Hash32>) -> VerifyOutcome {
        verify_asset(m, CIPHERTEXT, &self.directory, &self.authority, head)
    }
}

#[test]
fn accept_valid_create() {
    let f = Fixture::new();
    assert_eq!(f.verify(&f.valid_create(), None), VerifyOutcome::Accept);
}

#[test]
fn accept_valid_non_create_chaining_to_head() {
    let f = Fixture::new();
    let create = f.valid_create();
    // Pretend the create is the local head; a metadata-update chains onto it.
    let head = crate::crypto::hash::hash_bytes(b"pretend-head");
    let update = f
        .core(Action::MetadataUpdate, Some(head))
        .sign(&f.device, &f.write1)
        .unwrap();
    assert_eq!(f.verify(&update, Some(head)), VerifyOutcome::Accept);
    // And the create itself accepts when the asset is unknown locally.
    assert_eq!(f.verify(&create, None), VerifyOutcome::Accept);
}

/// A mock authority whose admin chain does not verify — also exercises the trait seam
/// (verify_asset works against any `AlbumAuthority`, not just `ReferenceAuthority`).
struct UntrustedAuthorityMock(Uuid);
impl AlbumAuthority for UntrustedAuthorityMock {
    fn album_id(&self) -> Uuid {
        self.0
    }
    fn epoch_ceiling(&self) -> AmkVersion {
        AmkVersion(10)
    }
    fn write_tier_pubkey(&self, _: AmkVersion) -> Option<crate::crypto::keys::HybridVerifyingKey> {
        None
    }
    fn has_amk(&self, _: AmkVersion) -> bool {
        true
    }
    fn admin_chain_verifies(&self) -> bool {
        false
    }
}

#[test]
fn reject_untrusted_authority() {
    let f = Fixture::new();
    let bad = UntrustedAuthorityMock(Uuid::from_u128(ALBUM));
    assert_eq!(
        verify_asset(&f.valid_create(), CIPHERTEXT, &f.directory, &bad, None),
        VerifyOutcome::TerminalReject(RejectReason::UntrustedAuthority)
    );
}

#[test]
fn reject_wrong_album() {
    let f = Fixture::new();
    let mut core = f.core(Action::Create, None);
    core.album_id = Uuid::from_u128(0xBEEF);
    let m = core.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::WrongAlbum)
    );
}

#[test]
fn reject_unknown_suite_downgrade() {
    let f = Fixture::new();
    let mut core = f.core(Action::Create, None);
    core.crypto_suite_id = 0xFFFF; // unknown suite, signed validly
    let m = core.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::SuiteDowngrade)
    );
}

#[test]
fn flipping_suite_after_signing_breaks_device_sig() {
    let f = Fixture::new();
    let mut m = f.valid_create();
    // Change the declared suite without re-signing: signing bytes diverge → sig fails.
    m.core.crypto_suite_id = CRYPTO_SUITE_ID; // still known, but sig was over the original
    m.core.protocol_version = "1999-01-01".into();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
}

#[test]
fn reject_structural_non_create_with_null_prior() {
    let f = Fixture::new();
    let m = f
        .core(Action::Replace, None)
        .sign(&f.device, &f.write1)
        .unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::Structural)
    );
}

#[test]
fn reject_ciphertext_hash_mismatch() {
    let f = Fixture::new();
    let m = f.valid_create();
    // Verify against different ciphertext bytes than the manifest committed to.
    assert_eq!(
        verify_asset(&m, b"different bytes", &f.directory, &f.authority, None),
        VerifyOutcome::TerminalReject(RejectReason::CiphertextHashMismatch)
    );
}

#[test]
fn reject_unknown_device() {
    let f = Fixture::new();
    let mut core = f.core(Action::Create, None);
    core.created_by_device = Uuid::from_u128(0xDEAD);
    let m = core.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::UnknownDevice)
    );
}

#[test]
fn reject_device_added_after_manifest() {
    let f = Fixture::new();
    let mut core = f.core(Action::Create, None);
    // Manifest claims a time before the device was added to the directory.
    core.timestamp = "2026-05-29T00:00:00Z".into();
    let m = core.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::DeviceAddedAfter)
    );
}

#[test]
fn reject_bad_device_sig() {
    let f = Fixture::new();
    let mut m = f.valid_create();
    m.device_sig = HybridSigningKey::from_seed_bytes(&[99; 32], &[99; 32]).sign(b"garbage");
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
}

// ── Directory dispatch on the declared classical algorithm ────────────

/// The chokepoint accepts a manifest whose signing device is a **hardware P-256** hybrid
/// key: the directory entry declares `EcdsaP256`, and `verify_asset` dispatches to the
/// P-256 verify path. Proves the Ed25519-only assumption is gone from the chokepoint.
#[test]
fn accept_valid_create_from_p256_device() {
    use std::sync::Arc;

    use crate::crypto::keys::p256::MockP256Element;
    use crate::crypto::keys::{P256HybridSigningKey, Signer as _};

    let f = Fixture::new();
    let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
    let device = P256HybridSigningKey::enroll(
        Arc::new(MockP256Element::new([7; 32], false)),
        "device-dsk".into(),
        &[2; 32],
    )
    .unwrap();
    let directory = DirectoryCore {
        user_id: Uuid::from_u128(USER),
        directory_version: 1,
        updated_at: "2026-05-30T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: Uuid::from_u128(DEVICE),
            dsk_public: device.verifying_key(),
            dek_public: None,
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    }
    .sign(&ik);

    // Sign the manifest with the P-256 device (device_sig) and the epoch-1 write key.
    let m = f
        .core(Action::Create, None)
        .sign(&device, &f.write1)
        .unwrap();
    assert_eq!(
        verify_asset(&m, CIPHERTEXT, &directory, &f.authority, None),
        VerifyOutcome::Accept
    );

    // Cross-algorithm forgery: an Ed25519 device signature against the P-256 directory entry
    // is rejected at the algorithm-dispatch guard (declared EcdsaP256 ≠ signature Ed25519).
    let ed_signed = f
        .core(Action::Create, None)
        .sign(&f.device, &f.write1)
        .unwrap();
    assert_eq!(
        verify_asset(&ed_signed, CIPHERTEXT, &directory, &f.authority, None),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
}

#[test]
fn reject_reader_signed_no_write_capability() {
    let f = Fixture::new();
    // A reader holds the device key but NOT a write-tier key — sign write_sig with a
    // non-write key. The write_sig won't verify under the epoch's attested write key.
    let reader_fake_write = HybridSigningKey::from_seed_bytes(&[77; 32], &[78; 32]);
    let m = f
        .core(Action::Create, None)
        .sign(&f.device, &reader_fake_write)
        .unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::BadWriteSig)
    );
}

#[test]
fn reject_removed_writer_wrong_epoch_key() {
    let f = Fixture::new();
    // Signer holds epoch-2's write key but claims epoch 1 (e.g. a writer removed at the
    // epoch-1→2 bump trying to pass off old work). write_sig won't verify under epoch 1's key.
    let m = f
        .core(Action::Create, None)
        .sign(&f.device, &f.write2)
        .unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::BadWriteSig)
    );
}

#[test]
fn reject_wrong_epoch_above_ceiling() {
    let f = Fixture::new();
    let mut core = f.core(Action::Create, None);
    core.amk_version = AmkVersion(99); // above the attested ceiling of 2
    let m = core.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::WrongEpoch)
    );
}

#[test]
fn reject_forged_chain_stale_prior() {
    let f = Fixture::new();
    let head = hash::hash_bytes(b"real-head");
    let stale = hash::hash_bytes(b"stale-or-forked");
    let m = f
        .core(Action::Delete, Some(stale))
        .sign(&f.device, &f.write1)
        .unwrap();
    // Local head is `head`, but the manifest chains to `stale`.
    assert_eq!(
        f.verify(&m, Some(head)),
        VerifyOutcome::TerminalReject(RejectReason::ForgedChain)
    );
}

#[test]
fn reject_replayed_create_when_asset_exists() {
    let f = Fixture::new();
    let existing = hash::hash_bytes(b"existing-head");
    assert_eq!(
        f.verify(&f.valid_create(), Some(existing)),
        VerifyOutcome::TerminalReject(RejectReason::Replayed)
    );
}

// ── Pending semantics ────────────────────────────────────────────────────────

#[test]
fn pending_when_amk_within_range_but_not_local() {
    // Authority attests epoch 1's write key but the AMK content key hasn't arrived.
    let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
    let write1 = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
    let directory = DirectoryCore {
        user_id: Uuid::from_u128(USER),
        directory_version: 1,
        updated_at: "2026-05-30T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: Uuid::from_u128(DEVICE),
            dsk_public: device.verifying_key(),
            dek_public: None,
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    }
    .sign(&ik);
    let mut authority = ReferenceAuthority::new(Uuid::from_u128(ALBUM), admin.verifying_key())
        .with_epoch(
            &admin,
            AmkVersion(1),
            &write1.verifying_key(),
            false, // AMK NOT yet locally held
        );

    let f = Fixture::new();
    let m = f.core(Action::Create, None).sign(&device, &write1).unwrap();
    assert_eq!(
        verify_asset(&m, CIPHERTEXT, &directory, &authority, None),
        VerifyOutcome::Pending(PendingReason::AmkNotYetLocal),
        "valid but AMK-missing manifest must be pending, not rejected"
    );

    // When the AMK arrives, the same manifest now accepts.
    authority.mark_amk_present(AmkVersion(1));
    assert_eq!(
        verify_asset(&m, CIPHERTEXT, &directory, &authority, None),
        VerifyOutcome::Accept
    );
}

// ── Wrapped file-key mode (adopted web-upload drop) ──────────────────────────

/// The doc's wrapped-mode positive case: a `key_mode = wrapped` create verifies, and a
/// member holding the AMK unwraps `wrapped_file_key` to recover the guest-chosen file key
/// and STREAM-decrypts the unchanged ciphertext. Authorization is unchanged from derived.
#[test]
fn wrapped_mode_member_unwraps_and_decrypts() {
    let f = Fixture::new();
    let amk = Amk::from_bytes([0x5A; 32]);
    let file_id = Uuid::from_u128(0xF11E);
    // A guest chose a random file key K and STREAM-encrypted the asset under it.
    let k = [0x77u8; 32];
    let plaintext = b"adopted web-upload drop bytes";
    let (enc, ct) = encrypt_asset_vec_with_prefix(&k, [1, 2, 3, 4, 5, 6, 7], plaintext);

    let mut c = f.core(Action::Create, None);
    c.key_mode = KeyMode::Wrapped;
    c.wrapped_file_key = Some(WrappedFileKey(seal_file_key(&amk, &file_id, &k)));
    c.ciphertext_hash = enc.ciphertext_hash;
    c.nonce_prefix = enc.nonce_prefix;
    let m = c.sign(&f.device, &f.write1).unwrap();

    // verify_asset accepts the wrapped manifest exactly like a derived one.
    assert_eq!(
        verify_asset(&m, &ct, &f.directory, &f.authority, None),
        VerifyOutcome::Accept
    );
    // The AMK-holding member recovers K and decrypts.
    let recovered =
        unseal_file_key(&amk, &file_id, &m.core.wrapped_file_key.as_ref().unwrap().0).unwrap();
    assert_eq!(recovered, k);
    assert_eq!(
        decrypt_asset_vec(&recovered, &enc.nonce_prefix, &ct).unwrap(),
        plaintext
    );
}

/// The doc's wrapped-mode negative case: altering `wrapped_file_key` (or `key_mode`
/// itself) after signing fails `verify_asset` like any other tampered signed field —
/// both are covered by both signatures, so tampering diverges the signing bytes.
#[test]
fn wrapped_mode_tampering_a_signed_field_terminally_rejects() {
    let f = Fixture::new();
    // A valid wrapped-mode create (bound to the fixture's CIPHERTEXT for the hash check).
    let mut c = f.core(Action::Create, None);
    c.key_mode = KeyMode::Wrapped;
    c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; 60]));
    let signed = c.sign(&f.device, &f.write1).unwrap();
    assert_eq!(f.verify(&signed, None), VerifyOutcome::Accept);

    // Tamper the wrapped key after signing → device_sig no longer verifies.
    let mut tampered = signed.clone();
    tampered.core.wrapped_file_key = Some(WrappedFileKey(vec![0xCD; 60]));
    assert_eq!(
        f.verify(&tampered, None),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );

    // Flip the mode back to derived (dropping the wrapped key to keep it structurally
    // well-formed) → still a signed-field divergence → terminal reject.
    let mut flipped = signed.clone();
    flipped.core.key_mode = KeyMode::Derived;
    flipped.core.wrapped_file_key = None;
    assert!(flipped.structural_ok(), "flip is structurally well-formed");
    assert_eq!(
        f.verify(&flipped, None),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
}

/// A `key_mode = wrapped` manifest missing its `wrapped_file_key` (or a derived one that
/// carries one) is structurally rejected before any signature check.
#[test]
fn wrapped_mode_presence_mismatch_is_structural() {
    let f = Fixture::new();
    // Wrapped but no wrapped_file_key.
    let mut c = f.core(Action::Create, None);
    c.key_mode = KeyMode::Wrapped;
    c.wrapped_file_key = None;
    let m = c.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::Structural)
    );

    // Derived but carrying a wrapped_file_key.
    let mut c = f.core(Action::Create, None);
    c.key_mode = KeyMode::Derived;
    c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; 60]));
    let m = c.sign(&f.device, &f.write1).unwrap();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::TerminalReject(RejectReason::Structural)
    );
}

#[test]
fn drop_in_authority_parity() {
    // The seam is honored only through &dyn AlbumAuthority: a second authority that
    // attests nothing rejects the same manifest at the epoch check, proving verify_asset
    // depends only on the trait, not on ReferenceAuthority internals.
    let f = Fixture::new();
    let empty = ReferenceAuthority::new(Uuid::from_u128(ALBUM), f.admin.verifying_key());
    let dynamic: &dyn AlbumAuthority = &empty;
    assert_eq!(
        verify_asset(&f.valid_create(), CIPHERTEXT, &f.directory, dynamic, None),
        VerifyOutcome::TerminalReject(RejectReason::WrongEpoch)
    );
}

// ── Metadata↔manifest binding (invariant 25 client half) ───────────────

use crate::crypto::encryption::seal_metadata_blob;

/// Seal `sidecar` into a metadata blob (fresh nonce folded into the derived blob key) and
/// produce a create manifest committing to the blob's content hash. Returns
/// `(manifest, blob, blob_key)`.
fn sealed_create(f: &Fixture, sidecar: &[u8]) -> (AssetManifest, Vec<u8>, [u8; 32]) {
    let amk = Amk::from_bytes([0x5A; 32]);
    let file_id = Uuid::from_u128(0xF11E);
    let (blob, blob_key) = seal_metadata_blob(&amk, &file_id, sidecar, None).unwrap();
    let mut c = f.core(Action::Create, None);
    c.metadata_blob_hash = Some(blob_ciphertext_hash(&blob));
    let m = c.sign(&f.device, &f.write1).unwrap();
    (m, blob, blob_key)
}

/// The doc's positive case: a metadata blob decrypts to the byte-identical signed sidecar
/// and its content hash equals the manifest's `metadata_blob_hash`.
#[test]
fn metadata_binding_round_trips() {
    let f = Fixture::new();
    let sidecar = b"canonical CBOR sidecar bytes for the asset";
    let (m, blob, blob_key) = sealed_create(&f, sidecar);
    assert_eq!(
        verify_metadata_binding(&m, &blob, &blob_key, sidecar),
        MetadataBinding::Bound
    );
}

/// A one-byte mutation of the *local* sidecar breaks the round-trip: the blob decrypts to
/// the original bytes, which no longer match. Quarantined, never persisted.
#[test]
fn metadata_binding_one_byte_sidecar_mutation_quarantines() {
    let f = Fixture::new();
    let sidecar = b"canonical CBOR sidecar bytes for the asset".to_vec();
    let (m, blob, blob_key) = sealed_create(&f, &sidecar);
    let mut tampered = sidecar.clone();
    tampered[0] ^= 0x01;
    assert_eq!(
        verify_metadata_binding(&m, &blob, &blob_key, &tampered),
        MetadataBinding::Quarantine(BindingReject::SidecarMismatch),
    );
}

/// A one-byte mutation of the *blob* breaks the content-address half first (the key-free
/// check the server also runs): the hash no longer matches the committed value.
#[test]
fn metadata_binding_one_byte_blob_mutation_quarantines() {
    let f = Fixture::new();
    let sidecar = b"canonical CBOR sidecar bytes for the asset";
    let (m, blob, blob_key) = sealed_create(&f, sidecar);
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert_eq!(
        verify_metadata_binding(&m, &tampered, &blob_key, sidecar),
        MetadataBinding::Quarantine(BindingReject::BlobHashMismatch),
    );
}

/// The blob hashes correctly but the wrong key can't decrypt it → `Undecryptable`
/// (distinct from a hash mismatch — the content address held, the AEAD did not).
#[test]
fn metadata_binding_undecryptable_under_wrong_key() {
    let f = Fixture::new();
    let sidecar = b"canonical CBOR sidecar bytes for the asset";
    let (m, blob, _blob_key) = sealed_create(&f, sidecar);
    assert_eq!(
        verify_metadata_binding(&m, &blob, &[0u8; 32], sidecar),
        MetadataBinding::Quarantine(BindingReject::Undecryptable),
    );
}

/// A manifest that commits to no `metadata_blob_hash` (e.g. a caller mis-invoking on a
/// delete) yields `NoManifestCommitment`, never a false `Bound`.
#[test]
fn metadata_binding_requires_a_manifest_commitment() {
    let f = Fixture::new();
    // A delete manifest carries no metadata_blob_hash.
    let head = hash::hash_bytes(b"prior");
    let m = f
        .core(Action::Delete, Some(head))
        .sign(&f.device, &f.write1)
        .unwrap();
    assert_eq!(
        verify_metadata_binding(&m, b"whatever", &[0u8; 32], b"whatever"),
        MetadataBinding::Quarantine(BindingReject::NoManifestCommitment),
    );
}

// ── Legacy inherited-device records (issue #475) ────────────────────────────

const SECOND_DEVICE: u128 = 0xD2;

/// The fixture's account with a second device published, plus that device's DSK — a user
/// with a phone and a laptop.
fn with_second_device(f: &mut Fixture) -> HybridSigningKey {
    with_second_device_added_at(f, "2026-05-30T00:00:00Z")
}

/// [`with_second_device`], with the second device published at `added_at`.
fn with_second_device_added_at(f: &mut Fixture, added_at: &str) -> HybridSigningKey {
    let second = HybridSigningKey::from_seed_bytes(&[21; 32], &[22; 32]);
    let mut core = f.directory.core.clone();
    core.directory_version += 1;
    core.devices.push(DeviceEntry {
        device_id: Uuid::from_u128(SECOND_DEVICE),
        dsk_public: second.verifying_key(),
        dek_public: None,
        added_at: added_at.into(),
        revoked_at: None,
        unknown: Default::default(),
    });
    f.directory = core.sign(&HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]));
    second
}

fn diagnose(f: &Fixture, m: &AssetManifest, head: Option<Hash32>) -> Option<Uuid> {
    diagnose_misattributed_signer(
        m,
        &hash::hash_bytes(CIPHERTEXT),
        &f.directory,
        &f.authority,
        head,
    )
}

/// The defect exactly as the old `sign_lifecycle` wrote it: a continuation that names the
/// creating device (inherited from the chain head) but is signed by the second device. The
/// chokepoint still refuses it — and the diagnosis names the device that really signed.
#[test]
fn an_inherited_device_continuation_is_refused_and_diagnosed() {
    let mut f = Fixture::new();
    let second = with_second_device(&mut f);
    let head = hash::hash_bytes(b"create-head");
    // `created_by_device` is the fixture's DEVICE (the creator); the signer is `second`.
    let m = f
        .core(Action::Delete, Some(head))
        .sign(&second, &f.write1)
        .unwrap();

    assert_eq!(
        f.verify(&m, Some(head)),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig),
        "the misattributed record is never accepted"
    );
    assert_eq!(
        diagnose(&f, &m, Some(head)),
        Some(Uuid::from_u128(SECOND_DEVICE))
    );
}

/// A genuine forgery — a signature under no key the directory publishes — is not diagnosed:
/// it keeps the plain `BadDeviceSig` signal.
#[test]
fn a_forged_device_sig_is_not_diagnosed_as_legacy() {
    let mut f = Fixture::new();
    with_second_device(&mut f);
    let head = hash::hash_bytes(b"create-head");
    let forger = HybridSigningKey::from_seed_bytes(&[66; 32], &[67; 32]);
    let m = f
        .core(Action::Delete, Some(head))
        .sign(&forger, &f.write1)
        .unwrap();
    assert_eq!(
        f.verify(&m, Some(head)),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
    assert_eq!(diagnose(&f, &m, Some(head)), None);
}

/// A device published after the record's timestamp cannot have written it: its
/// signature is not diagnosed as the legacy defect, even though it verifies and every
/// other step holds.
#[test]
fn a_signer_added_after_the_record_is_not_diagnosed() {
    let mut f = Fixture::new();
    // The record is stamped 2026-05-31T12:00:00Z; the second device arrives a day later.
    let second = with_second_device_added_at(&mut f, "2026-06-01T00:00:00Z");
    let head = hash::hash_bytes(b"create-head");
    let m = f
        .core(Action::Delete, Some(head))
        .sign(&second, &f.write1)
        .unwrap();
    assert_eq!(
        f.verify(&m, Some(head)),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
    assert_eq!(diagnose(&f, &m, Some(head)), None);
}

/// The diagnosis holds the true signer to every other step: a misattributed record that
/// also lacks album write authority, sits at the wrong chain position, or is a `create`
/// is not the legacy defect and is not diagnosed as it.
#[test]
fn the_diagnosis_requires_every_other_step_to_hold() {
    let mut f = Fixture::new();
    let second = with_second_device(&mut f);
    let head = hash::hash_bytes(b"create-head");

    // Wrong write-tier key for the claimed epoch (a removed writer).
    let unauthorized = f
        .core(Action::Delete, Some(head))
        .sign(&second, &f.write2)
        .unwrap();
    assert_eq!(diagnose(&f, &unauthorized, Some(head)), None);

    // Right signatures, wrong chain position (stale / replayed continuation).
    let stale = f
        .core(Action::Delete, Some(hash::hash_bytes(b"stale")))
        .sign(&second, &f.write1)
        .unwrap();
    assert_eq!(diagnose(&f, &stale, Some(head)), None);

    // A `create` always named its signer; a misattributed one is not this defect.
    let create = f
        .core(Action::Create, None)
        .sign(&second, &f.write1)
        .unwrap();
    assert_eq!(
        f.verify(&create, None),
        VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig)
    );
    assert_eq!(diagnose(&f, &create, None), None);

    // A correctly attributed record is accepted, never "diagnosed".
    let fine = f
        .core(Action::Delete, Some(head))
        .sign(&f.device, &f.write1)
        .unwrap();
    assert_eq!(f.verify(&fine, Some(head)), VerifyOutcome::Accept);
    assert_eq!(diagnose(&f, &fine, Some(head)), None);
}
