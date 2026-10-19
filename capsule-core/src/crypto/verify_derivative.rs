//! The derivative-manifest arm of the acknowledgement chokepoint (SSoT: [Cryptography —
//! Provenance § Derivative Provenance]).
//!
//! [`verify_derivative`] is the one path by which a client trusts a received
//! [`DerivativeManifest`] — the manifest that names the epoch, the source asset and the nonce
//! prefix a thumbnail or preview is decrypted with. "A derivative whose manifest fails
//! verification is surfaced, never silently applied": it answers with the same three-way
//! [`VerifyOutcome`] [`verify_asset`](crate::crypto::verify_asset::verify_asset) does, and holds a
//! derivative to the same two signatures:
//!
//! - `device_sig` under the generating device's entry in the published directory, dispatched on
//!   the entry's declared classical algorithm;
//! - `write_sig` under the album's write-tier key for the epoch `amk_version` selects — through
//!   the very helper `verify_asset` uses, so a derivative can never be authorized by a weaker
//!   rule than an asset.
//!
//! Two bindings an asset manifest does not need are added: `source_asset_id` must name the asset
//! the derivative is being opened for (a validly signed derivative of *another* asset is a
//! transplant), and `prior_provenance_hash` must chain to the receiver's head for
//! `(source_asset_id, role)` — the derivative-poisoning defense.
//!
//! [Cryptography — Provenance § Derivative Provenance]: https://docs/design/cryptography/provenance/#derivative-provenance

use uuid::Uuid;

use crate::crypto::authority::AlbumAuthority;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::DeviceDirectory;
use crate::crypto::primitives::SuiteId;
use crate::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeManifest};
use crate::crypto::verify_asset::{
    PendingReason, RejectReason, VerifyOutcome, authorize_write, rfc3339_le,
};

/// Verify a received derivative manifest for the asset `asset_id`.
///
/// - `directory` is the published directory of the account whose device generated the
///   derivative; `generated_by_device` is resolved in it.
/// - `role_head` is the [`link_hash`](DerivativeManifest::link_hash) of the manifest this
///   receiver already trusts as the head of `(asset_id, role)`, or `None` when it trusts none
///   yet. A manifest whose `prior_provenance_hash` is `None` starts the role's chain and is only
///   placed when there is no head; any other must name the head exactly.
///
/// The derivative's ciphertext is not an argument: its content address is checked where the
/// bytes arrive (the blob fetch recomputes it against the address the manifest names), and a
/// manifest is verified before any byte is fetched.
///
/// Steps, in order — the first that fails decides the [`RejectReason`]:
///
/// 1. the authority's admin chain verifies ([`RejectReason::UntrustedAuthority`]);
/// 2. `version` is `derivative-manifest/v1`, and the two fields that are wire-optional only on
///    pre-binding fixtures — `amk_version` and `protocol_version` — are present
///    ([`RejectReason::Structural`]): without `amk_version` there is no key to check `write_sig`
///    under;
/// 3. `crypto_suite_id` is implemented by this build ([`RejectReason::SuiteDowngrade`]);
/// 4. `source_asset_id` is `asset_id` ([`RejectReason::WrongAsset`]);
/// 5. `generated_by_device` is in `directory` ([`RejectReason::UnknownDevice`]) and was added no
///    later than `generated_at` ([`RejectReason::DeviceAddedAfter`] /
///    [`RejectReason::BadTimestamp`]);
/// 6. `device_sig` verifies under that entry's key and declared algorithm
///    ([`RejectReason::BadDeviceSig`]);
/// 7. the epoch is within the attested ceiling and `write_sig` verifies under its write-tier key
///    ([`RejectReason::WrongEpoch`] / [`RejectReason::BadWriteSig`]);
/// 8. chain placement against `role_head` ([`RejectReason::Replayed`] for a chain start when a
///    head exists, [`RejectReason::ForgedChain`] for any other mismatch);
/// 9. accept iff the epoch's AMK is held locally, else
///    [`VerifyOutcome::Pending`]`(`[`PendingReason::AmkNotYetLocal`]`)`.
#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(%asset_id, role = ?manifest.core.role, device = %manifest.core.generated_by_device)
)]
pub fn verify_derivative(
    manifest: &DerivativeManifest,
    asset_id: Uuid,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    role_head: Option<Hash32>,
) -> VerifyOutcome {
    let outcome = verify_steps(manifest, asset_id, directory, authority, role_head);
    match outcome {
        VerifyOutcome::Accept => tracing::trace!("derivative manifest accepted"),
        VerifyOutcome::Pending(reason) => tracing::debug!(?reason, "derivative manifest pending"),
        VerifyOutcome::TerminalReject(reason) => {
            tracing::debug!(?reason, "derivative manifest rejected");
        }
    }
    outcome
}

fn verify_steps(
    manifest: &DerivativeManifest,
    asset_id: Uuid,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    role_head: Option<Hash32>,
) -> VerifyOutcome {
    use RejectReason::{
        BadDeviceSig, BadTimestamp, DeviceAddedAfter, ForgedChain, Replayed, Structural,
        SuiteDowngrade, UnknownDevice, UntrustedAuthority, WrongAsset,
    };
    use VerifyOutcome::TerminalReject as Reject;
    let core = &manifest.core;

    // 1. Never trust an unsigned ledger.
    if !authority.admin_chain_verifies() {
        return Reject(UntrustedAuthority);
    }
    // 2. Schema, and the two fields every real write carries.
    let Some(amk_version) = core.amk_version else {
        return Reject(Structural);
    };
    if core.version != DERIVATIVE_MANIFEST_VERSION || core.protocol_version.is_none() {
        return Reject(Structural);
    }
    // 3. Fail closed on a suite this build does not implement.
    if SuiteId::from_u16(core.crypto_suite_id).is_none() {
        return Reject(SuiteDowngrade);
    }
    // 4. A derivative of another asset is a transplant, however well it is signed.
    if core.source_asset_id != asset_id {
        return Reject(WrongAsset);
    }
    // 5. The generating device is published, and was published before it generated this.
    let Some(entry) = directory.device(&core.generated_by_device) else {
        return Reject(UnknownDevice);
    };
    match rfc3339_le(&entry.added_at, &core.generated_at) {
        None => return Reject(BadTimestamp),
        Some(false) => return Reject(DeviceAddedAfter),
        Some(true) => {}
    }
    // 6. Provenance: the device signature, under the entry's declared classical algorithm.
    let signing_bytes = manifest.signing_bytes();
    if manifest.device_sig.classical_algorithm() != entry.dsk_public.classical_algorithm()
        || !entry
            .dsk_public
            .verify(&signing_bytes, &manifest.device_sig)
    {
        return Reject(BadDeviceSig);
    }
    // 7. Authorization: the epoch write-tier signature, by the rule assets are held to.
    if let Some(reason) =
        authorize_write(amk_version, &signing_bytes, &manifest.write_sig, authority)
    {
        return Reject(reason);
    }
    // 8. Chain placement per (asset, role): the derivative-poisoning defense.
    match (core.prior_provenance_hash, role_head) {
        (None, None) => {}
        (None, Some(_)) => return Reject(Replayed),
        (Some(prior), Some(head)) if prior == head => {}
        (Some(_), _) => return Reject(ForgedChain),
    }
    // 9. Everything authorizes; accept iff the epoch's AMK is here.
    if authority.has_amk(amk_version) {
        VerifyOutcome::Accept
    } else {
        VerifyOutcome::Pending(PendingReason::AmkNotYetLocal)
    }
}

#[cfg(test)]
mod tests {
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
            }],
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
}
