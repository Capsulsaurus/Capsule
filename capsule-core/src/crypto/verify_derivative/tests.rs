use std::sync::Arc;

use super::*;
use crate::crypto::authority::ReferenceAuthority;
use crate::crypto::hash;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::p256::MockP256Element;
use crate::crypto::keys::{AmkVersion, HybridSigningKey, P256HybridSigningKey, Signer as _};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::{DerivativeCore, DerivativeRole};

const USER: u128 = 0x05E2;
const DEVICE: u128 = 0xD1;
const ALBUM: u128 = 0xA1;
const ASSET: u128 = 0xF11E;

/// A fully valid setup; each test perturbs exactly one thing.
struct Fixture {
    device: HybridSigningKey,
    write1: HybridSigningKey,
    write2: HybridSigningKey,
    directory: DeviceDirectory,
    authority: ReferenceAuthority,
}

fn directory_with(dsk: crate::crypto::keys::HybridVerifyingKey) -> DeviceDirectory {
    DirectoryCore {
        user_id: Uuid::from_u128(USER),
        directory_version: 1,
        updated_at: "2026-05-30T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: Uuid::from_u128(DEVICE),
            dsk_public: dsk,
            dek_public: None,
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    }
    .sign(&HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]))
}

impl Fixture {
    fn new() -> Self {
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write1 = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let write2 = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]);
        let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
        let authority = ReferenceAuthority::new(Uuid::from_u128(ALBUM), admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &write1.verifying_key(), true)
            .with_epoch(&admin, AmkVersion(2), &write2.verifying_key(), true);
        Self {
            directory: directory_with(device.verifying_key()),
            device,
            write1,
            write2,
            authority,
        }
    }

    fn core(&self, role: DerivativeRole, prior: Option<Hash32>) -> DerivativeCore {
        DerivativeCore {
            version: DERIVATIVE_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: Some(PROTOCOL_VERSION.into()),
            amk_version: Some(AmkVersion(1)),
            source_asset_id: Uuid::from_u128(ASSET),
            role,
            format: "image/jxl".into(),
            ciphertext_hash: hash::hash_bytes(b"derivative ciphertext"),
            nonce_prefix: [9, 8, 7, 6, 5, 4, 3],
            generated_by_device: Uuid::from_u128(DEVICE),
            generated_by_client: "capsule-cli/0.1.0".into(),
            model_id: None,
            model_version: None,
            generated_at: "2026-05-31T12:00:00Z".into(),
            prior_provenance_hash: prior,
        }
    }

    fn sign(&self, core: DerivativeCore) -> DerivativeManifest {
        core.sign(&self.device, &self.write1)
            .expect("software signers sign")
    }

    fn first(&self) -> DerivativeManifest {
        self.sign(self.core(DerivativeRole::Thumbnail, None))
    }

    fn verify(&self, m: &DerivativeManifest, head: Option<Hash32>) -> VerifyOutcome {
        verify_derivative(
            m,
            Uuid::from_u128(ASSET),
            &self.directory,
            &self.authority,
            head,
        )
    }
}

fn rejects(reason: RejectReason) -> VerifyOutcome {
    VerifyOutcome::TerminalReject(reason)
}

// ── Positive cases ───────────────────────────────────────────────────────────

#[test]
fn accepts_the_first_derivative_of_a_role() {
    let f = Fixture::new();
    assert_eq!(f.verify(&f.first(), None), VerifyOutcome::Accept);
}

#[test]
fn accepts_a_replace_that_chains_to_the_role_head() {
    let f = Fixture::new();
    let head = f.first();
    let replace = f.sign(f.core(DerivativeRole::Thumbnail, Some(head.link_hash())));
    assert_eq!(
        f.verify(&replace, Some(head.link_hash())),
        VerifyOutcome::Accept
    );
}

/// The chain link is the producer's: SHA-256 over the manifest's canonical CBOR, so a
/// manifest received as its exact bytes links identically to the one that was signed.
#[test]
fn the_link_hash_is_the_digest_of_the_canonical_bytes() {
    let f = Fixture::new();
    let m = f.first();
    let bytes = crate::cbor::to_canonical_vec(&m).expect("serializes");
    assert_eq!(m.link_hash(), hash::hash_bytes(&bytes));
    let back: DerivativeManifest = crate::cbor::from_slice(&bytes).expect("decodes");
    assert_eq!(back.link_hash(), m.link_hash());
}

#[test]
fn accepts_a_hardware_p256_device() {
    let f = Fixture::new();
    let device = P256HybridSigningKey::enroll(
        Arc::new(MockP256Element::new([7; 32], false)),
        "device-dsk".into(),
        &[2; 32],
    )
    .expect("mock element enrolls");
    let directory = directory_with(device.verifying_key());
    let m = f
        .core(DerivativeRole::Preview, None)
        .sign(&device, &f.write1)
        .expect("signs");
    let verify = |m: &DerivativeManifest| {
        verify_derivative(m, Uuid::from_u128(ASSET), &directory, &f.authority, None)
    };
    assert_eq!(verify(&m), VerifyOutcome::Accept);
    // An Ed25519 device signature against the P-256 entry is a cross-algorithm forgery.
    assert_eq!(verify(&f.first()), rejects(RejectReason::BadDeviceSig));
}

#[test]
fn pending_while_the_epoch_amk_is_not_local() {
    let mut f = Fixture::new();
    let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
    f.authority = ReferenceAuthority::new(Uuid::from_u128(ALBUM), admin.verifying_key())
        .with_epoch(&admin, AmkVersion(1), &f.write1.verifying_key(), false);
    let m = f.first();
    assert_eq!(
        f.verify(&m, None),
        VerifyOutcome::Pending(PendingReason::AmkNotYetLocal)
    );
    f.authority.mark_amk_present(AmkVersion(1));
    assert_eq!(f.verify(&m, None), VerifyOutcome::Accept);
}

// ── Negative cases: one per damage scenario ─────────────────────────────────

#[test]
fn rejects_an_untrusted_authority() {
    struct Untrusted;
    impl AlbumAuthority for Untrusted {
        fn album_id(&self) -> Uuid {
            Uuid::from_u128(ALBUM)
        }
        fn epoch_ceiling(&self) -> AmkVersion {
            AmkVersion(10)
        }
        fn write_tier_pubkey(
            &self,
            _: AmkVersion,
        ) -> Option<crate::crypto::keys::HybridVerifyingKey> {
            None
        }
        fn has_amk(&self, _: AmkVersion) -> bool {
            true
        }
        fn admin_chain_verifies(&self) -> bool {
            false
        }
    }
    let f = Fixture::new();
    assert_eq!(
        verify_derivative(
            &f.first(),
            Uuid::from_u128(ASSET),
            &f.directory,
            &Untrusted,
            None
        ),
        rejects(RejectReason::UntrustedAuthority)
    );
}

/// The contract's own words: "a derivative without it cannot be authorization-verified and
/// is rejected".
#[test]
fn rejects_a_manifest_without_an_epoch() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.amk_version = None;
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::Structural)
    );
}

#[test]
fn rejects_a_manifest_without_a_protocol_version() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.protocol_version = None;
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::Structural)
    );
}

#[test]
fn rejects_an_unknown_schema_version() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.version = "derivative-manifest/v0".into();
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::Structural)
    );
}

#[test]
fn rejects_an_unknown_suite() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.crypto_suite_id = 0xFFFF;
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::SuiteDowngrade)
    );
}

/// A validly signed derivative of another asset, served on this asset's entry.
#[test]
fn rejects_a_derivative_transplanted_from_another_asset() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.source_asset_id = Uuid::from_u128(0xBEEF);
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::WrongAsset)
    );
}

#[test]
fn rejects_an_unknown_device() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.generated_by_device = Uuid::from_u128(0xDEAD);
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::UnknownDevice)
    );
}

#[test]
fn rejects_a_device_added_after_generation() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.generated_at = "2026-05-29T00:00:00Z".into();
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::DeviceAddedAfter)
    );
}

#[test]
fn rejects_an_unparseable_generation_time() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.generated_at = "yesterday".into();
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::BadTimestamp)
    );
}

#[test]
fn rejects_a_forged_device_signature() {
    let f = Fixture::new();
    let mut m = f.first();
    m.device_sig = HybridSigningKey::from_seed_bytes(&[99; 32], &[99; 32]).sign(b"garbage");
    assert_eq!(f.verify(&m, None), rejects(RejectReason::BadDeviceSig));
}

/// Every signed field is covered: altering one after signing breaks `device_sig` first.
#[test]
fn rejects_a_field_tampered_after_signing() {
    let f = Fixture::new();
    let mut m = f.first();
    m.core.nonce_prefix = [0; 7];
    assert_eq!(f.verify(&m, None), rejects(RejectReason::BadDeviceSig));
}

/// A reader holds a device key but no write-tier key.
#[test]
fn rejects_a_reader_signed_derivative() {
    let f = Fixture::new();
    let reader = HybridSigningKey::from_seed_bytes(&[77; 32], &[78; 32]);
    let m = f
        .core(DerivativeRole::Thumbnail, None)
        .sign(&f.device, &reader)
        .expect("signs");
    assert_eq!(f.verify(&m, None), rejects(RejectReason::BadWriteSig));
}

/// Signed with epoch 2's write key while claiming epoch 1.
#[test]
fn rejects_a_write_signature_from_the_wrong_epoch() {
    let f = Fixture::new();
    let m = f
        .core(DerivativeRole::Thumbnail, None)
        .sign(&f.device, &f.write2)
        .expect("signs");
    assert_eq!(f.verify(&m, None), rejects(RejectReason::BadWriteSig));
}

/// "Both halves required": the write signature is checked by `HybridVerifyingKey::verify`,
/// whose own tests corrupt each half alone. Here the whole signature comes from a different
/// manifest, so neither half covers these bytes.
#[test]
fn rejects_a_write_signature_lifted_from_another_manifest() {
    let f = Fixture::new();
    let mut m = f.first();
    m.write_sig = f.sign(f.core(DerivativeRole::Preview, None)).write_sig;
    assert_eq!(f.verify(&m, None), rejects(RejectReason::BadWriteSig));
}

#[test]
fn rejects_an_epoch_above_the_attested_ceiling() {
    let f = Fixture::new();
    let mut core = f.core(DerivativeRole::Thumbnail, None);
    core.amk_version = Some(AmkVersion(99));
    assert_eq!(
        f.verify(&f.sign(core), None),
        rejects(RejectReason::WrongEpoch)
    );
}

/// **Derivative poisoning rejection** (provenance.md § Validation): a `derivative-replace`
/// whose `prior_provenance_hash` does not chain to the current head for `(asset_id, role)`
/// is rejected, so the head — the existing derivative — stays the one the receiver opens.
#[test]
fn rejects_a_replace_that_does_not_chain_to_the_role_head() {
    let f = Fixture::new();
    let head = f.first();
    let poison = f.sign(f.core(
        DerivativeRole::Thumbnail,
        Some(hash::hash_bytes(b"some other link")),
    ));
    assert_eq!(
        f.verify(&poison, Some(head.link_hash())),
        rejects(RejectReason::ForgedChain)
    );
    // A replace that names a head the receiver does not hold is equally unplaceable.
    assert_eq!(f.verify(&poison, None), rejects(RejectReason::ForgedChain));
    // The existing head still verifies in its place.
    assert_eq!(f.verify(&head, None), VerifyOutcome::Accept);
}

/// A second chain start for a role that already has a head is a replay of the first slot.
#[test]
fn rejects_a_second_chain_start_for_a_role() {
    let f = Fixture::new();
    let head = f.first();
    let restart = f.sign(f.core(DerivativeRole::Thumbnail, None));
    assert_eq!(
        f.verify(&restart, Some(head.link_hash())),
        rejects(RejectReason::Replayed)
    );
}
