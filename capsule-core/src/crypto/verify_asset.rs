//! The single asset-acknowledgement chokepoint (SSoT: [Keys — Write Authorization]).
//!
//! `verify_asset` is the **only** path by which a client accepts an asset into its trusted
//! set. It returns one of three outcomes — never a silent drop, never a silent accept:
//!
//! - [`VerifyOutcome::Accept`] — both signatures valid, epoch within the MLS-attested
//!   ceiling, chain head matches, AMK locally held.
//! - [`VerifyOutcome::TerminalReject`] — reader-signed / removed-writer / wrong-epoch /
//!   forged-chain / replayed / suite-downgrade / bad device sig … → quarantine.
//! - [`VerifyOutcome::Pending`] — epoch is within the attested range but its AMK content
//!   key has not arrived yet → hold and retry, never quarantine.
//!
//! Authorization authority is the album's admin-signed MLS commit chain (the
//! [`AlbumAuthority`]), never the server. It ships with an exhaustive negative-case test
//! surface, since every negative is a real damage scenario.
//!
//! [Keys — Write Authorization]: https://docs/design/cryptography/keys/#write-authorization

use crate::crypto::authority::AlbumAuthority;
use crate::crypto::encryption::{blob_ciphertext_hash, open_blob};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::DeviceDirectory;
use crate::crypto::primitives::SuiteId;
use crate::crypto::provenance::AssetManifest;

/// Why a manifest was terminally rejected. Each maps to a damage scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The album authority's admin chain does not verify — untrusted state.
    UntrustedAuthority,
    /// The manifest names a different album than this authority speaks for.
    WrongAlbum,
    /// `crypto_suite_id` is not in the current inventory (downgrade / unknown).
    SuiteDowngrade,
    /// Structural rule violated (e.g. non-create with null prior; retention off a delete).
    Structural,
    /// Recomputed ciphertext hash does not match the manifest's declared hash.
    CiphertextHashMismatch,
    /// The signing device is not in the user's published directory.
    UnknownDevice,
    /// The device's `added_at` postdates the manifest timestamp (key older than itself).
    DeviceAddedAfter,
    /// A timestamp field was not valid RFC3339.
    BadTimestamp,
    /// The device signature (`device_sig`) did not verify.
    BadDeviceSig,
    /// `amk_version` exceeds the MLS-attested epoch ceiling (fabricated future epoch).
    WrongEpoch,
    /// The write-tier signature did not verify for the claimed epoch (reader-signed,
    /// removed-writer, or wrong-epoch signature).
    BadWriteSig,
    /// `prior_provenance_hash` does not match the local chain head (stale / forked / replay).
    ForgedChain,
    /// A `create` for an asset that already exists locally (replay).
    Replayed,
}

/// Why a manifest is pending rather than accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReason {
    /// The epoch is attested but its AMK content key has not arrived locally yet.
    AmkNotYetLocal,
}

/// The outcome of [`verify_asset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Acknowledge the asset.
    Accept,
    /// Reject and quarantine, with a structured reason.
    TerminalReject(RejectReason),
    /// Hold and retry as MLS state catches up.
    Pending(PendingReason),
}

impl VerifyOutcome {
    /// Convenience: did verification accept?
    pub fn is_accept(self) -> bool {
        matches!(self, VerifyOutcome::Accept)
    }
}

fn rfc3339_le(a: &str, b: &str) -> Option<bool> {
    let pa = a.parse::<jiff::Timestamp>().ok()?;
    let pb = b.parse::<jiff::Timestamp>().ok()?;
    Some(pa <= pb)
}

/// Verify a manifest against the device directory, the album's MLS-attested authority, and
/// the local provenance chain head. The one path by which an asset is acknowledged.
///
/// - `ciphertext` is the asset's ciphertext, used to confirm the content hash (integrity);
///   it need not be decryptable here.
/// - `local_chain_head` is the hash of the current head provenance record for this asset,
///   or `None` if the asset is unknown locally.
pub fn verify_asset(
    manifest: &AssetManifest,
    ciphertext: &[u8],
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    local_chain_head: Option<Hash32>,
) -> VerifyOutcome {
    use RejectReason::{
        BadDeviceSig, BadTimestamp, BadWriteSig, CiphertextHashMismatch, DeviceAddedAfter,
        ForgedChain, Replayed, Structural, SuiteDowngrade, UnknownDevice, UntrustedAuthority,
        WrongAlbum, WrongEpoch,
    };
    use VerifyOutcome::TerminalReject as Reject;
    let core = &manifest.core;

    // 1. The authority's own admin chain must verify — never trust an unsigned ledger.
    if !authority.admin_chain_verifies() {
        return Reject(UntrustedAuthority);
    }
    // 2. The manifest must be for the album this authority speaks for.
    if core.album_id != authority.album_id() {
        return Reject(WrongAlbum);
    }
    // 3. The crypto suite must be one this build implements (fail-closed).
    if SuiteId::from_u16(core.crypto_suite_id).is_none() {
        return Reject(SuiteDowngrade);
    }
    // 4. Structural rules (prior-hash/create coupling; retention only on delete).
    if !manifest.structural_ok() {
        return Reject(Structural);
    }
    // 5. Content integrity: the ciphertext must hash to the declared content address.
    if hash::hash_bytes(ciphertext) != core.ciphertext_hash {
        return Reject(CiphertextHashMismatch);
    }
    // 6. The signing device must be in this user's published directory.
    if directory.core.user_id != core.created_by_user {
        return Reject(UnknownDevice);
    }
    let Some(entry) = directory.device(&core.created_by_device) else {
        return Reject(UnknownDevice);
    };
    // 7. The device cannot have been added after it claims to have signed.
    match rfc3339_le(&entry.added_at, &core.timestamp) {
        None => return Reject(BadTimestamp),
        Some(false) => return Reject(DeviceAddedAfter),
        Some(true) => {}
    }
    // 8. The device signature must verify under the entry's **declared classical algorithm**.
    //    The directory entry's `dsk_public` is algorithm-tagged — Ed25519 (software) or
    //    ECDSA-P256 (hardware secure elements) — so verification dispatches on that tag. A
    //    signature whose classical half is a *different* algorithm than the entry declares is a
    //    cross-algorithm forgery and is rejected before the (algorithm-specific) verify.
    let signing_bytes = manifest.signing_bytes();
    if manifest.device_sig.classical_algorithm() != entry.dsk_public.classical_algorithm() {
        return Reject(BadDeviceSig);
    }
    if !entry
        .dsk_public
        .verify(&signing_bytes, &manifest.device_sig)
    {
        return Reject(BadDeviceSig);
    }
    // 9. The epoch must be within the MLS-attested ceiling (no fabricated future epoch).
    if core.amk_version > authority.epoch_ceiling() {
        return Reject(WrongEpoch);
    }
    // 10. The write-tier signature must verify under the epoch's attested write-tier key.
    //     Reader-signed and removed-writer manifests both fail this check.
    let Some(write_pub) = authority.write_tier_pubkey(core.amk_version) else {
        return Reject(WrongEpoch);
    };
    if !write_pub.verify(&signing_bytes, &manifest.write_sig) {
        return Reject(BadWriteSig);
    }
    // 11. Chain placement: a create must be new; a non-create must chain to the local head.
    if core.action.is_create() {
        if local_chain_head.is_some() {
            return Reject(Replayed);
        }
    } else if core.prior_provenance_hash.is_none() || core.prior_provenance_hash != local_chain_head
    {
        return Reject(ForgedChain);
    }
    // 12. Everything authorizes; accept iff the AMK is locally held, else hold as pending.
    if authority.has_amk(core.amk_version) {
        VerifyOutcome::Accept
    } else {
        VerifyOutcome::Pending(PendingReason::AmkNotYetLocal)
    }
}

/// Why a metadata blob fails to bind to its manifest and the locally-signed sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingReject {
    /// The manifest commits to no `metadata_blob_hash` — a metadata-bearing action must
    /// (`create | replace | metadata-update`); a caller should not seek a binding otherwise.
    NoManifestCommitment,
    /// The blob's content hash does not equal the manifest's committed `metadata_blob_hash`.
    /// This is the key-free half — the same comparison the server runs as [invariant 25].
    ///
    /// [invariant 25]: crate::validation::check_metadata_blob_envelope
    BlobHashMismatch,
    /// The blob did not decrypt/authenticate under the derived blob key (tampered ciphertext,
    /// wrong key, or a truncated wire).
    Undecryptable,
    /// The blob decrypted, but its plaintext is not byte-identical to the locally-signed
    /// sidecar — the client would be persisting one sidecar while its manifest commits to a
    /// different one.
    SidecarMismatch,
}

/// The outcome of the metadata↔manifest round-trip binding check (client-side; SSoT:
/// [Metadata — Local and Server Metadata Equivalence]).
///
/// This is a **distinct class** from a [`verify_asset`] terminal-reject: a manifest's two
/// signatures can be perfectly valid while the metadata blob a client was handed diverges from
/// what the manifest commits to. Per the [client-side invariant] a divergence is
/// **quarantined** — surfaced, never silently persisted — exactly like the pending→timeout and
/// terminal-reject cases funnel into quarantine, but reached from a different check.
///
/// [Metadata — Local and Server Metadata Equivalence]: https://docs/design/metadata/#local-and-server-metadata-equivalence
/// [client-side invariant]: https://docs/design/threat-model/validation/#client-side-validation-invariants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataBinding {
    /// The blob decrypts to the byte-identical signed sidecar and hashes to the manifest field.
    Bound,
    /// The blob diverges from the manifest or the signed sidecar — quarantine, never persist.
    Quarantine(BindingReject),
}

impl MetadataBinding {
    /// Convenience: did the metadata blob bind?
    pub fn is_bound(self) -> bool {
        matches!(self, MetadataBinding::Bound)
    }
}

/// Run the metadata↔manifest round-trip equivalence check for a metadata-bearing manifest
/// (SSoT: [Metadata — Local and Server Metadata Equivalence], [Encryption — Local–server
/// equivalence]). Two facts must hold, both funnelling a failure into a
/// [quarantine](MetadataBinding::Quarantine):
///
/// 1. **Content address** — the metadata blob's content hash equals the manifest's committed
///    `metadata_blob_hash` (the key-free half; the server enforces the same value at
///    [invariant 25](crate::validation::check_metadata_blob_envelope)).
/// 2. **Round-trip** — decrypting the blob under `blob_key` yields canonical CBOR
///    byte-identical to `signed_sidecar` (the sidecar the client persists locally).
///
/// A client therefore never persists a sidecar that does not round-trip to the committed
/// `metadata_blob_hash`.
///
/// [Metadata — Local and Server Metadata Equivalence]: https://docs/design/metadata/#local-and-server-metadata-equivalence
/// [Encryption — Local–server equivalence]: https://docs/design/cryptography/encryption/#metadata-encryption
pub fn verify_metadata_binding(
    manifest: &AssetManifest,
    metadata_blob: &[u8],
    blob_key: &[u8; 32],
    signed_sidecar: &[u8],
) -> MetadataBinding {
    use MetadataBinding::Quarantine;

    let Some(committed) = manifest.core.metadata_blob_hash else {
        return Quarantine(BindingReject::NoManifestCommitment);
    };
    // (1) Key-free content address: the ciphertext blob hashes to the committed value.
    if blob_ciphertext_hash(metadata_blob) != committed {
        return Quarantine(BindingReject::BlobHashMismatch);
    }
    // (2) Round-trip: the blob decrypts to the byte-identical signed sidecar.
    match open_blob(blob_key, metadata_blob) {
        Err(_) => Quarantine(BindingReject::Undecryptable),
        Ok(plaintext) if plaintext != signed_sidecar => Quarantine(BindingReject::SidecarMismatch),
        Ok(_) => MetadataBinding::Bound,
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::crypto::authority::ReferenceAuthority;
    use crate::crypto::encryption::stream::{decrypt_asset_vec, encrypt_asset_vec_with_prefix};
    use crate::crypto::encryption::{seal_file_key, unseal_file_key};
    use crate::crypto::hash::Hash32;
    use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
    use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::action::Action;
    use crate::crypto::provenance::manifest::{
        ASSET_MANIFEST_VERSION, KeyMode, ManifestCore, WrappedFileKey,
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
                }],
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
                metadata_blob_hash: action.binds_metadata_blob().then(|| Hash32([0x4D; 32])),
                created_by_user: Uuid::from_u128(USER),
                created_by_device: Uuid::from_u128(DEVICE),
                client_version: "capsule-cli/0.1.0".into(),
                timestamp: "2026-05-31T12:00:00Z".into(),
                action,
                prior_provenance_hash: prior,
                upgraded_from: None,
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

    // ── Directory dispatch on the declared classical algorithm (S-A4) ────────────

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
            }],
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
            }],
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

    // ── Metadata↔manifest binding (S-A3, invariant 25 client half) ───────────────

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
}
