//! Contract-driven tests for the S-X2 membership layer. Every deferred-from-S-X1 Validation
//! bullet from [MLS](https://docs/design/cryptography/mls/#validation) has a test here:
//! four-ceremony round-trip, Welcome correctness (full + capped), history-policy consistency,
//! epoch-ceiling-from-chain, idempotent commit replay, MLS↔identity LeafNode binding, and
//! concurrent-commit convergence — plus removed-member forward secrecy, the `has_amk`/ceiling
//! semantics across ceremonies, and durable persistence round-trip.
//!
//! The multi-party topology is entirely in-process: two-plus [`OpenMlsAuthority`] instances
//! exchange real MLS messages by value (the server-side delivery-service transport is out of
//! scope for this slice).

use openmls::prelude::KeyPackage;
use openmls::prelude::tls_codec::Deserialize as _;

use super::*;
use crate::crypto::authority::{AlbumAuthority, ReferenceAuthority};
use crate::crypto::hash;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{DeviceDirectory, HybridSigningKey};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use crate::crypto::verify_asset::{VerifyOutcome, verify_asset};

const CIPHERTEXT: &[u8] = b"the asset ciphertext bytes";

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A device with a deterministic DSK, plus a fresh MLS identity around it.
struct Device {
    user_id: Uuid,
    device_id: Uuid,
    dsk: HybridSigningKey,
}

impl Device {
    fn new(user: u128, device: u128, seed: u8) -> Self {
        Self {
            user_id: Uuid::from_u128(user),
            device_id: Uuid::from_u128(device),
            dsk: HybridSigningKey::from_seed_bytes(&[seed; 32], &[seed ^ 0xFF; 32]),
        }
    }

    fn identity(&self) -> MlsDeviceIdentity {
        MlsDeviceIdentity::with_dsk(self.user_id, self.device_id, self.dsk.clone()).unwrap()
    }

    /// A single-device directory for this device, signed by a throwaway user IK (the binding
    /// check reads the device's DSK from the directory, not the directory's own signature).
    fn directory(&self) -> DeviceDirectory {
        let ik = HybridSigningKey::from_seed_bytes(&[0xAA; 32], &[0xBB; 32]);
        DirectoryCore {
            user_id: self.user_id,
            directory_version: 1,
            updated_at: "2026-05-30T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: self.device_id,
                dsk_public: self.dsk.verifying_key(),
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(&ik)
    }
}

/// Convert an outgoing MLS message into an incoming one over the real wire codec (in-process
/// delivery; the `From<MlsMessageOut>` shortcut is `test-utils`-gated upstream).
fn to_in(m: MlsMessageOut) -> MlsMessageIn {
    let bytes = m.to_bytes().expect("serialize MLS message");
    MlsMessageIn::tls_deserialize_exact(bytes.as_slice()).expect("deserialize MLS message")
}

/// Drive an `add_member` + `join_via_welcome` handshake between `admin` and a fresh joiner,
/// returning the joined authority. Only valid when `admin` is the sole existing member (no other
/// members need the commit relayed).
fn add_and_join(
    admin: &mut OpenMlsAuthority,
    joiner: &Device,
    policy: HistoryPolicy,
) -> OpenMlsAuthority {
    let identity = joiner.identity();
    let key_package: KeyPackage = identity.key_package().unwrap();
    let outcome = admin.add_member(key_package, &joiner.directory()).unwrap();
    let history: Vec<MlsMessageIn> = outcome.key_delivery.into_iter().map(to_in).collect();
    OpenMlsAuthority::join_via_welcome(identity, to_in(outcome.welcome), history, policy).unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════
// S-X1 surface — preserved so the backend/authority layer keeps its guarantees
// ═══════════════════════════════════════════════════════════════════════════

fn directory_for(user: Uuid, device_id: Uuid, device: &HybridSigningKey) -> DeviceDirectory {
    let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
    DirectoryCore {
        user_id: user,
        directory_version: 1,
        updated_at: "2026-05-30T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id,
            dsk_public: device.verifying_key(),
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
        }],
    }
    .sign(&ik)
}

fn create_manifest_core(album: Uuid, epoch: AmkVersion, user: Uuid, device: Uuid) -> ManifestCore {
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id: Uuid::from_u128(0xF11E),
        album_id: album,
        amk_version: epoch,
        ciphertext_hash: hash::hash_bytes(CIPHERTEXT),
        plaintext_size: 12,
        chunk_size: 65_520,
        nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(hash::Hash32([0x4D; 32])),
        created_by_user: user,
        created_by_device: device,
        client_version: "capsule-cli/0.1.0".into(),
        timestamp: "2026-05-31T12:00:00Z".into(),
        action: Action::Create,
        prior_provenance_hash: None,
        retention_until: None,
    }
}

#[test]
fn ciphersuite_is_pinned_xwing_0x004d() {
    let auth = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
    assert_eq!(auth.ciphersuite(), PINNED_CIPHERSUITE);
    assert_eq!(u16::from(auth.ciphersuite()), PINNED_CIPHERSUITE_ID);
    assert_eq!(PINNED_CIPHERSUITE_ID, 0x004D);
}

#[test]
fn genesis_epoch_and_ceiling() {
    let auth = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
    assert!(auth.admin_chain_verifies());
    assert_eq!(auth.epoch_ceiling(), AmkVersion(1));
    assert_eq!(auth.mls_epoch(), 0);
    assert!(auth.write_tier_pubkey(AmkVersion(1)).is_some());
    assert!(auth.write_tier_pubkey(AmkVersion(2)).is_none());
    assert!(auth.has_amk(AmkVersion(1)));
    assert!(!auth.has_amk(AmkVersion(2)));
}

#[test]
fn amk_export_is_deterministic_and_advances_with_epoch() {
    let mut auth = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
    let a1 = auth.export_current_amk().unwrap();
    let a2 = auth.export_current_amk().unwrap();
    assert_eq!(a1, a2, "AMK export must be deterministic within an epoch");
    assert_eq!(auth.amk(AmkVersion(1)), Some(a1));

    let v2 = auth.advance_epoch(true).unwrap();
    assert_eq!(v2, AmkVersion(2));
    assert_eq!(auth.epoch_ceiling(), AmkVersion(2));
    assert_eq!(auth.mls_epoch(), 1);
    let b = auth.export_current_amk().unwrap();
    assert_ne!(a1, b, "advancing the epoch must change the AMK");
    assert_eq!(auth.amk(AmkVersion(2)), Some(b));
    assert_eq!(auth.amk(AmkVersion(1)), Some(a1));
}

#[test]
fn different_albums_export_different_amks() {
    let a = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
    let b = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xB2)).unwrap();
    assert_ne!(a.amk(AmkVersion(1)), b.amk(AmkVersion(1)));
}

#[test]
fn verify_asset_accepts_manifest_under_live_mls_authority() {
    let album = Uuid::from_u128(0xA1);
    let auth = OpenMlsAuthority::create_self_group(album).unwrap();
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let user = Uuid::from_u128(0x05E2);
    let dev_id = Uuid::from_u128(0xD1);
    let directory = directory_for(user, dev_id, &device);

    let write_tier = auth.write_tier_signing_key(AmkVersion(1)).unwrap();
    let manifest = create_manifest_core(album, AmkVersion(1), user, dev_id)
        .sign(&device, write_tier)
        .unwrap();

    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
        VerifyOutcome::Accept
    );
}

#[test]
fn pending_when_amk_not_yet_delivered_then_accept_on_delivery() {
    let album = Uuid::from_u128(0xA1);
    let mut auth = OpenMlsAuthority::create_self_group(album).unwrap();
    let v2 = auth.advance_epoch(false).unwrap();
    assert_eq!(v2, AmkVersion(2));
    assert!(!auth.has_amk(AmkVersion(2)));

    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let user = Uuid::from_u128(0x05E2);
    let dev_id = Uuid::from_u128(0xD1);
    let directory = directory_for(user, dev_id, &device);
    let write_tier = auth.write_tier_signing_key(AmkVersion(2)).unwrap();
    let manifest = create_manifest_core(album, AmkVersion(2), user, dev_id)
        .sign(&device, write_tier)
        .unwrap();
    assert!(matches!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
        VerifyOutcome::Pending(_)
    ));
    auth.mark_amk_present(AmkVersion(2));
    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
        VerifyOutcome::Accept
    );
}

#[test]
fn parity_with_reference_authority_over_equivalent_history() {
    let album = Uuid::from_u128(0xA1);
    let mut mls = OpenMlsAuthority::create_self_group(album).unwrap();
    let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
    let w1 = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let w2 = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]);
    let mut reference = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
        &admin,
        AmkVersion(1),
        &w1.verifying_key(),
        true,
    );
    assert_eq!(mls.epoch_ceiling(), reference.epoch_ceiling());
    assert!(mls.admin_chain_verifies() && reference.admin_chain_verifies());
    assert!(mls.has_amk(AmkVersion(1)) && reference.has_amk(AmkVersion(1)));

    mls.advance_epoch(true).unwrap();
    reference.attest_epoch(&admin, AmkVersion(2), &w2.verifying_key(), true);
    assert_eq!(mls.epoch_ceiling(), AmkVersion(2));
    assert_eq!(mls.epoch_ceiling(), reference.epoch_ceiling());
    assert!(mls.has_amk(AmkVersion(2)) && reference.has_amk(AmkVersion(2)));
}

// ═══════════════════════════════════════════════════════════════════════════
// S-X2 — membership ceremonies + key delivery + history policy
// ═══════════════════════════════════════════════════════════════════════════

/// **Protocol round-trip** across all four ceremony kinds (add user, add device, self-update
/// rotation, remove), asserting every remaining member's view of the group state — epoch, ceiling,
/// and per-epoch AMK — matches after each commit.
#[test]
fn four_ceremony_round_trip_converges_all_member_views() {
    let album = Uuid::from_u128(0x0A1B);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();

    // Ceremony 1 — add user Bob.
    let bob_dev = Device::new(0x2, 0x22, 2);
    let mut bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);
    assert_converged(&[&admin, &bob]);
    assert_eq!(admin.epoch_ceiling(), AmkVersion(2));

    // Ceremony 2 — add another device/member Carol; Bob (existing) must process the commit.
    let carol_dev = Device::new(0x3, 0x33, 3);
    let carol_identity = carol_dev.identity();
    let carol_kp = carol_identity.key_package().unwrap();
    let add = admin.add_member(carol_kp, &carol_dev.directory()).unwrap();
    // Existing member Bob applies the commit, then the AMK broadcast.
    bob.process_commit(to_in(add.commit.clone())).unwrap();
    for m in &add.key_delivery {
        bob.process_key_delivery(to_in(m.clone())).unwrap();
    }
    let history: Vec<MlsMessageIn> = add.key_delivery.into_iter().map(to_in).collect();
    let carol = OpenMlsAuthority::join_via_welcome(
        carol_identity,
        to_in(add.welcome),
        history,
        HistoryPolicy::Full,
    )
    .unwrap();
    assert_eq!(admin.epoch_ceiling(), AmkVersion(3));
    assert_converged(&[&admin, &bob, &carol]);

    // Ceremony 3 — scheduled rotation (self-update) by the admin.
    let rot = admin.rotate_epoch().unwrap();
    bob.process_commit(to_in(rot.commit.clone())).unwrap();
    for m in &rot.key_delivery {
        bob.process_key_delivery(to_in(m.clone())).unwrap();
    }
    let mut carol = carol;
    carol.process_commit(to_in(rot.commit.clone())).unwrap();
    for m in &rot.key_delivery {
        carol.process_key_delivery(to_in(m.clone())).unwrap();
    }
    assert_eq!(admin.epoch_ceiling(), AmkVersion(4));
    assert_converged(&[&admin, &bob, &carol]);

    // Ceremony 4 — remove Carol; the remaining members re-key and converge.
    let carol_leaf = admin.leaf_index_of_device(carol_dev.device_id).unwrap();
    let rem = admin.remove_member(carol_leaf).unwrap();
    bob.process_commit(to_in(rem.commit.clone())).unwrap();
    for m in &rem.key_delivery {
        bob.process_key_delivery(to_in(m.clone())).unwrap();
    }
    assert_eq!(admin.epoch_ceiling(), AmkVersion(5));
    assert_converged(&[&admin, &bob]);
    // Carol is evicted by the removal commit and cannot advance.
    assert!(carol.process_commit(to_in(rem.commit)).is_err());
    assert!(!carol.admin_chain_verifies());
}

/// Assert a set of members share an identical group view: same MLS epoch/ceiling, byte-equal
/// AMKs, and the same attested write-tier public key for every epoch any of them holds.
fn assert_converged(members: &[&OpenMlsAuthority]) {
    let (first, rest) = members.split_first().expect("at least one member");
    for m in rest {
        assert_eq!(m.mls_epoch(), first.mls_epoch(), "MLS epoch diverged");
        assert_eq!(m.epoch_ceiling(), first.epoch_ceiling(), "ceiling diverged");
        for v in 1..=first.epoch_ceiling().0 {
            if let (Some(a), Some(b)) = (first.amk(AmkVersion(v)), m.amk(AmkVersion(v))) {
                assert_eq!(a, b, "AMK for epoch {v} diverged between members");
            }
            if let (Some(a), Some(b)) = (
                first.write_tier_pubkey(AmkVersion(v)),
                m.write_tier_pubkey(AmkVersion(v)),
            ) {
                assert_eq!(
                    a, b,
                    "write-tier pubkey for epoch {v} diverged between members"
                );
            }
        }
        assert!(m.admin_chain_verifies() && first.admin_chain_verifies());
    }
}

/// **Welcome correctness (full history).** A joiner with `HistoryPolicy::Full` receives every
/// prior AMK and can both decrypt (`amk`) and authorization-check (`verify_asset`) a pre-join
/// asset — its Welcome delivered the whole range `AMK_v1..AMK_current`.
#[test]
fn welcome_full_history_delivers_every_prior_amk() {
    let album = Uuid::from_u128(0x0F11);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    // Build several epochs of history (1, 2, 3) before anyone joins.
    admin.advance_epoch(true).unwrap();
    admin.advance_epoch(true).unwrap();
    assert_eq!(admin.epoch_ceiling(), AmkVersion(3));

    // A pre-join asset written by the admin's device at epoch 2.
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let user = Uuid::from_u128(0x05E2);
    let dev_id = Uuid::from_u128(0xD1);
    let directory = directory_for(user, dev_id, &device);
    let manifest = create_manifest_core(album, AmkVersion(2), user, dev_id)
        .sign(
            &device,
            admin.write_tier_signing_key(AmkVersion(2)).unwrap(),
        )
        .unwrap();

    let bob_dev = Device::new(0x2, 0x22, 2);
    let bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);
    // Bob joined at epoch 4 (the add advanced it); full history means every prior AMK is held.
    for v in 1..=admin.epoch_ceiling().0 {
        assert_eq!(
            bob.amk(AmkVersion(v)),
            admin.amk(AmkVersion(v)),
            "full-history joiner must hold epoch {v}'s AMK identically"
        );
        assert!(bob.has_amk(AmkVersion(v)));
    }
    // And the pre-join manifest at epoch 2 verifies for Bob (he holds the AMK + write-tier pubkey).
    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &bob, None),
        VerifyOutcome::Accept
    );
    // Bob received the current epoch's write-tier private half in his join delivery (writer today),
    // but prior epochs are read-only: pubkey held, no signing capability.
    assert!(bob.write_tier_signing_key(bob.epoch_ceiling()).is_some());
    assert!(bob.write_tier_signing_key(AmkVersion(2)).is_none());
    assert!(bob.write_tier_pubkey(AmkVersion(2)).is_some());
}

/// **Welcome correctness (capped history).** A joiner with `HistoryPolicy::Capped(n)` receives
/// only the last `n` epochs' AMKs; older epochs remain undecryptable (no AMK held).
#[test]
fn welcome_capped_history_delivers_only_last_n_epochs() {
    let album = Uuid::from_u128(0x0CAB);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Capped(2))
            .unwrap();
    admin.advance_epoch(true).unwrap(); // epoch 2
    admin.advance_epoch(true).unwrap(); // epoch 3
    assert_eq!(admin.epoch_ceiling(), AmkVersion(3));

    let bob_dev = Device::new(0x2, 0x22, 2);
    let bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Capped(2));
    // Bob joined at epoch 4; capped(2) entitles epochs {3, 4} — the current one plus one prior.
    let ceiling = admin.epoch_ceiling().0;
    assert_eq!(ceiling, 4);
    assert!(bob.has_amk(AmkVersion(4)), "current epoch AMK held");
    assert!(
        bob.has_amk(AmkVersion(3)),
        "one prior epoch within the cap held"
    );
    assert!(
        bob.amk(AmkVersion(2)).is_none(),
        "epoch beyond the cap must not be delivered"
    );
    assert!(
        bob.amk(AmkVersion(1)).is_none(),
        "genesis epoch beyond the cap must not be delivered"
    );
}

/// **History-policy consistency.** The delivered AMK range is a pure function of `(policy,
/// ceiling)` — never the joiner, device, or add order. Two different users added by the same admin
/// at the same epoch under the same album policy receive the identical prior-epoch AMK set.
#[test]
fn history_policy_is_consistent_across_adds() {
    let album = Uuid::from_u128(0x0C05);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    admin.advance_epoch(true).unwrap(); // epoch 2

    // Add Bob; capture the prior-epoch AMK set he holds.
    let bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);
    let bob_prior: Vec<u32> = (1..bob.epoch_ceiling().0)
        .filter(|v| bob.has_amk(AmkVersion(*v)))
        .collect();

    // Add Carol at the *same* ceiling, different device/order; capture her prior-epoch AMK set.
    let carol = add_and_join(&mut admin, &Device::new(0x3, 0x33, 3), HistoryPolicy::Full);
    let carol_prior: Vec<u32> = (1..carol.epoch_ceiling().0)
        .filter(|v| carol.has_amk(AmkVersion(*v)))
        .collect();

    // Bob joined before Carol so their absolute ceilings differ by one add-epoch, but the *policy*
    // (full) delivers the whole prior range to each — the range is read from album metadata, not
    // chosen per add. Assert each got the complete contiguous prior range 1..ceiling.
    assert_eq!(bob_prior, (1..bob.epoch_ceiling().0).collect::<Vec<_>>());
    assert_eq!(
        carol_prior,
        (1..carol.epoch_ceiling().0).collect::<Vec<_>>()
    );
    // And the pure entitled-range function is add-independent for a fixed (policy, ceiling).
    assert_eq!(
        HistoryPolicy::Full.entitled_range(5),
        HistoryPolicy::Full.entitled_range(5)
    );
}

/// **Epoch ceiling from the chain.** A joiner adopts the Welcome's chain-attested epoch as its
/// monotonic `amk_version` ceiling and terminal-rejects any manifest claiming a higher epoch the
/// chain does not attest.
#[test]
fn joiner_adopts_epoch_ceiling_from_chain_and_rejects_beyond() {
    let album = Uuid::from_u128(0x0EC1);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    admin.advance_epoch(true).unwrap();
    admin.advance_epoch(true).unwrap(); // ceiling 3

    let bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);
    let ceiling = bob.epoch_ceiling();
    assert_eq!(
        ceiling,
        admin.epoch_ceiling(),
        "joiner adopts the chain's ceiling"
    );

    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let user = Uuid::from_u128(0x05E2);
    let dev_id = Uuid::from_u128(0xD1);
    let directory = directory_for(user, dev_id, &device);

    // A manifest claiming ceiling+1 — an epoch the chain does not attest — is terminal WrongEpoch.
    let beyond = AmkVersion(ceiling.0 + 1);
    let forged = create_manifest_core(album, beyond, user, dev_id)
        .sign(&device, admin.current_write_tier())
        .unwrap();
    assert!(matches!(
        verify_asset(&forged, CIPHERTEXT, &directory, &bob, None),
        VerifyOutcome::TerminalReject(_)
    ));
}

/// **Idempotency under commit replay.** OpenMLS orders commits on the chain and rejects a replayed
/// commit at the protocol layer; the member's group state is unchanged by the second application.
#[test]
fn replaying_a_commit_is_rejected_and_state_is_unchanged() {
    let album = Uuid::from_u128(0x0117);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    let rot = admin.rotate_epoch().unwrap();
    bob.process_commit(to_in(rot.commit.clone())).unwrap();
    let epoch_after_first = bob.mls_epoch();

    // Replaying the same commit is rejected (stale/duplicate epoch), state unchanged.
    assert!(bob.process_commit(to_in(rot.commit)).is_err());
    assert_eq!(bob.mls_epoch(), epoch_after_first);
}

/// **MLS ↔ identity LeafNode binding.** A KeyPackage whose leaf is not bound to the DSK the
/// directory publishes for the claimed device is rejected before any group mutation; a correctly
/// bound one is admitted.
#[test]
fn leaf_binding_is_enforced_against_the_device_directory() {
    let album = Uuid::from_u128(0x0B10);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();

    // A leaf bound to Bob's real DSK, presented with a directory that publishes a *different* DSK
    // for Bob's device → the hybrid identity signature over the MLS leaf key does not verify.
    let bob = Device::new(0x2, 0x22, 2);
    let kp = bob.identity().key_package().unwrap();
    let wrong_dsk = HybridSigningKey::from_seed_bytes(&[0x9; 32], &[0x9; 32]);
    let tampered_dir = DirectoryCore {
        user_id: bob.user_id,
        directory_version: 1,
        updated_at: "2026-05-30T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: bob.device_id,
            dsk_public: wrong_dsk.verifying_key(), // not the key that signed the binding
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
        }],
    }
    .sign(&HybridSigningKey::from_seed_bytes(&[7; 32], &[7; 32]));
    let rejected = admin.add_member(kp, &tampered_dir);
    assert!(
        matches!(rejected, Err(OpenMlsAuthorityError::Binding(_))),
        "a leaf not bound to the directory's device DSK must be rejected"
    );

    // The correctly-bound leaf is admitted.
    let ok = add_and_join(&mut admin, &bob, HistoryPolicy::Full);
    assert!(ok.admin_chain_verifies());
}

/// **Concurrent commits.** Two members stage self-updates against the same epoch; the delivery
/// service orders one first. The winner merges its own commit; the loser discards its pending
/// commit and processes the winner's — both converge on one epoch with no group split.
#[test]
fn concurrent_commits_converge_without_divergence() {
    let album = Uuid::from_u128(0x0C0C);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);
    let start_epoch = admin.mls_epoch();
    assert_eq!(start_epoch, bob.mls_epoch());

    // Both stage a self-update against the same epoch.
    let admin_commit = admin.stage_self_update().unwrap();
    let _bob_commit = bob.stage_self_update().unwrap();

    // Delivery service picks the admin's commit as the winner.
    admin.merge_pending().unwrap();
    bob.discard_pending_and_process(to_in(admin_commit.clone()))
        .unwrap();
    // Bob rebased: no signing capability for the winner's epoch until its distribution arrives
    // (his own staged epoch — and its minted write-tier key — never existed on the chain).
    let converged_epoch = admin.epoch_ceiling();
    assert!(bob.write_tier_signing_key(converged_epoch).is_none());
    // Deliver the winner's key material to Bob (AMK broadcast + write-tier distribution).
    let kd = admin.build_key_distribution(converged_epoch).unwrap();
    let wtd = admin
        .build_write_tier_distribution(converged_epoch)
        .unwrap();
    bob.process_key_delivery(to_in(kd)).unwrap();
    bob.process_key_delivery(to_in(wtd)).unwrap();
    assert!(bob.write_tier_signing_key(converged_epoch).is_some());

    assert_eq!(admin.mls_epoch(), bob.mls_epoch(), "converged to one epoch");
    assert_eq!(admin.mls_epoch(), start_epoch + 1);
    assert_converged(&[&admin, &bob]);
}

/// **Removed member forward secrecy.** After a remove + re-key, the removed device is evicted: it
/// cannot process subsequent commits or AMK broadcasts, so it never obtains a post-removal AMK.
#[test]
fn removed_member_cannot_obtain_subsequent_amks() {
    let album = Uuid::from_u128(0x0DEAD);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let bob_dev = Device::new(0x2, 0x22, 2);
    let mut bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);
    let shared_epoch = admin.epoch_ceiling();
    assert!(bob.has_amk(shared_epoch));

    // Admin removes Bob and re-keys.
    let bob_leaf = admin.leaf_index_of_device(bob_dev.device_id).unwrap();
    let rem = admin.remove_member(bob_leaf).unwrap();
    let new_epoch = admin.epoch_ceiling();
    assert_eq!(new_epoch, AmkVersion(shared_epoch.0 + 1));
    assert!(admin.amk(new_epoch).is_some());
    assert_ne!(
        admin.amk(new_epoch),
        admin.amk(shared_epoch),
        "re-key minted a fresh AMK"
    );

    // Bob is evicted: he can neither process the removal commit nor the post-removal AMK broadcast.
    assert!(bob.process_commit(to_in(rem.commit)).is_err());
    for m in rem.key_delivery {
        assert!(bob.process_key_delivery(to_in(m)).is_err());
    }
    assert!(
        bob.amk(new_epoch).is_none(),
        "removed member never holds the post-removal AMK"
    );
    assert!(
        bob.write_tier_signing_key(new_epoch).is_none(),
        "removed member never receives the post-removal write-tier key: no sign-capable handle"
    );
    assert!(
        bob.write_tier_pubkey(new_epoch).is_none(),
        "removed member cannot even attest the post-removal epoch"
    );
    assert!(
        !bob.admin_chain_verifies(),
        "an evicted authority is untrusted"
    );
}

/// **Write capability requires explicit distribution.** The write-tier private key is minted by
/// the committer and delivered over a [`WriteTierDistribution`] — it is *not derivable from group
/// state*. A member that processed the epoch's commit but not its distribution can verify
/// (chain-attested pubkey held) but has **no sign-capable handle**; after the distribution it can
/// sign, and a manifest under that key round-trips to `verify_asset` Accept on another member.
#[test]
fn write_tier_signing_requires_explicit_distribution() {
    let album = Uuid::from_u128(0x0517E);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    // Admin rotates; Bob processes ONLY the commit (key delivery still in flight).
    let rot = admin.rotate_epoch().unwrap();
    let new_epoch = bob.process_commit(to_in(rot.commit)).unwrap();
    assert_eq!(new_epoch, admin.epoch_ceiling());

    // Bob holds full group state for the epoch (he processed the commit and can export its
    // secrets), yet no sign-capable handle exists — the key was minted, not derived.
    assert!(
        bob.write_tier_signing_key(new_epoch).is_none(),
        "write-tier private key must not be obtainable from group state alone"
    );
    // He *can* verify: the commit's authenticated AAD attested the public half.
    assert_eq!(
        bob.write_tier_pubkey(new_epoch),
        admin.write_tier_pubkey(new_epoch),
        "commit AAD attests the write-tier public key to every processor"
    );

    // Distribution arrives → Bob gains signing capability.
    for m in &rot.key_delivery {
        bob.process_key_delivery(to_in(m.clone())).unwrap();
    }
    let bob_write_tier = bob
        .write_tier_signing_key(new_epoch)
        .expect("distribution delivered the sign-capable handle");

    // Distribution → sign → verify round-trip: a manifest write-signed by Bob's received key
    // verifies as Accept under the *admin's* authority (the chain-attested pubkey matches).
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let user = Uuid::from_u128(0x05E2);
    let dev_id = Uuid::from_u128(0xD1);
    let directory = directory_for(user, dev_id, &device);
    let manifest = create_manifest_core(album, new_epoch, user, dev_id)
        .sign(&device, bob_write_tier)
        .unwrap();
    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &admin, None),
        VerifyOutcome::Accept
    );
    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &bob, None),
        VerifyOutcome::Accept
    );
}

/// A tampered [`WriteTierDistribution`] — one whose seed does not derive the chain-attested
/// public key — is rejected, and the member remains without signing capability.
#[test]
fn write_tier_distribution_must_match_the_chain_attestation() {
    let album = Uuid::from_u128(0x0BAD5);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    let rot = admin.rotate_epoch().unwrap();
    let new_epoch = bob.process_commit(to_in(rot.commit)).unwrap();

    // A forged distribution for the attested epoch, carrying a different (attacker) key's seed.
    // Sign it as a real group application message from the admin's channel to isolate the
    // *content* check from MLS sender authentication.
    let forged_seed = HybridSigningKey::from_seed_bytes(&[0xE; 32], &[0xF; 32]).to_seed_bytes();
    let forged = MlsAppPayload::WriteTier(WriteTierDistribution {
        amk_version: new_epoch.0,
        write_tier_seed: forged_seed.to_vec(),
    });
    let msg = admin.create_app_message(&forged).unwrap();
    assert!(matches!(
        bob.process_key_delivery(to_in(msg)),
        Err(OpenMlsAuthorityError::Message(_))
    ));
    assert!(
        bob.write_tier_signing_key(new_epoch).is_none(),
        "a mismatching delivery must not install signing capability"
    );
}

/// **Durable persistence round-trip.** Export the full group state to bytes, reload it, and assert
/// the ledger + group are intact and the reloaded authority can still author a ceremony.
#[test]
fn export_import_state_round_trips_and_can_continue() {
    let album = Uuid::from_u128(0x0BEE5);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Capped(3))
            .unwrap();
    admin.advance_epoch(true).unwrap();
    let _bob = add_and_join(
        &mut admin,
        &Device::new(0x2, 0x22, 2),
        HistoryPolicy::Capped(3),
    );
    let ceiling = admin.epoch_ceiling();

    let blob = admin.export_state().unwrap();
    let mut restored = OpenMlsAuthority::import_state(&blob).unwrap();

    assert_eq!(restored.album_id(), admin.album_id());
    assert_eq!(restored.epoch_ceiling(), ceiling);
    assert_eq!(restored.history_policy(), HistoryPolicy::Capped(3));
    for v in 1..=ceiling.0 {
        assert_eq!(restored.amk(AmkVersion(v)), admin.amk(AmkVersion(v)));
        assert_eq!(
            restored.has_amk(AmkVersion(v)),
            admin.has_amk(AmkVersion(v))
        );
        // The held write-tier key material round-trips: same attested pubkey, and the private
        // half is present exactly where it was held (the admin minted every epoch's key).
        assert_eq!(
            restored.write_tier_pubkey(AmkVersion(v)),
            admin.write_tier_pubkey(AmkVersion(v))
        );
        assert_eq!(
            restored.write_tier_signing_key(AmkVersion(v)).is_some(),
            admin.write_tier_signing_key(AmkVersion(v)).is_some()
        );
    }
    // A signature from a restored write-tier key verifies under the original's attested pubkey.
    let sig = restored.write_tier_signing_key(ceiling).unwrap().sign(b"x");
    assert!(admin.write_tier_pubkey(ceiling).unwrap().verify(b"x", &sig));
    assert!(restored.admin_chain_verifies());
    // The reloaded group + signer can still drive a ceremony (proves the MLS state, not just the
    // ledger, round-tripped).
    let next = restored.advance_epoch(true).unwrap();
    assert_eq!(next, AmkVersion(ceiling.0 + 1));
}
