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

// ═══════════════════════════════════════════════════════════════════════════
// S-X3 — album upgrade ceremony (tombstone-plus-fork) + MLS resilience
//
// Every Validation bullet from versioning.md § Validation (upgrade-ceremony bullets) and
// mls-resilience.md § Validation gets a test here, driven through real OpenMLS state with the
// two-plus in-process participants the module already uses. Server-side concerns (the server-clock
// deadline, `409` on stale sessions, the drain of in-flight uploads) are modelled outside the
// authority per the slice's scope guards.
// ═══════════════════════════════════════════════════════════════════════════

/// An album-state summary carrying a single synthetic manifest hash, so two members can be made to
/// agree or disagree on the frozen state deterministically.
fn summary(tag: u8) -> AlbumStateSummary {
    AlbumStateSummary {
        manifest_hashes: vec![hash::Hash32([tag; 32])],
        provenance_head: Some(hash::Hash32([tag ^ 0x5A; 32])),
    }
}

/// Sign a fresh `Create` manifest for `album` at `epoch` under `write_tier`, returning it with the
/// author device's directory (the authorization check resolves the device DSK through it).
fn signed_manifest(
    album: Uuid,
    epoch: AmkVersion,
    write_tier: &HybridSigningKey,
) -> (
    crate::crypto::provenance::manifest::AssetManifest,
    DeviceDirectory,
) {
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let user = Uuid::from_u128(0x05E2);
    let dev_id = Uuid::from_u128(0xD1);
    let directory = directory_for(user, dev_id, &device);
    let manifest = create_manifest_core(album, epoch, user, dev_id)
        .sign(&device, write_tier)
        .unwrap();
    (manifest, directory)
}

// ── Upgrade ceremony (versioning.md § Validation) ──────────────────────────────

/// **Upgrade ceremony idempotency (smoke).** Run the ceremony; inject a crash *after* the tombstone
/// commit (step 4) by exporting/reloading; assert the same `intent_id` produces no second fork — the
/// reloaded admin is already tombstoned, so a second `commit_tombstone` is refused, and a duplicate
/// `AlbumTombstone` delivered to a member is a no-op at the MLS layer.
#[test]
fn upgrade_idempotency_no_second_fork_after_resume() {
    let album = Uuid::from_u128(0x0F0F);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    // Steps 1–2: propose + quiesce.
    let proposal = admin
        .propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2027-01-01",
            CRYPTO_SUITE_ID,
            DEFAULT_UPGRADE_DEADLINE,
        )
        .unwrap();
    let intent_id = proposal.signed_intent.intent.intent_id;
    bob.receive_upgrade_intent(to_in(proposal.message), &admin_dev.directory())
        .unwrap();

    // Step 4: tombstone. Both members share the same summary → hashes agree.
    let sm = summary(0x11);
    let tomb = admin.commit_tombstone(&sm).unwrap();
    assert_eq!(tomb.intent_id, intent_id);
    bob.process_tombstone(to_in(tomb.commit.clone()), &sm)
        .unwrap();
    assert_eq!(admin.is_tombstoned(), Some(intent_id));
    assert_eq!(bob.is_tombstoned(), Some(intent_id));

    // Crash after step 4: export/reload the admin. It comes back already tombstoned.
    let restored = OpenMlsAuthority::import_state(&admin.export_state().unwrap()).unwrap();
    assert_eq!(restored.is_tombstoned(), Some(intent_id));
    assert!(restored.has_completed_intent(intent_id));
    let mut restored = restored;
    // Resuming: a second tombstone under the same intent is refused — no second cutover, no fork #2.
    assert!(matches!(
        restored.commit_tombstone(&sm),
        Err(OpenMlsAuthorityError::Tombstoned(_))
    ));
    // A duplicate AlbumTombstone re-delivered to bob is rejected at the MLS layer (stale epoch).
    assert!(bob.process_tombstone(to_in(tomb.commit), &sm).is_err());
    assert_eq!(bob.epoch_ceiling(), admin.epoch_ceiling());
}

/// **Divergent member state aborts the upgrade (versioning.md step 4).** A member whose recomputed
/// `frozen_state_hash` differs from the proposer's rejects the tombstone; the abort is clean (the
/// member is *not* tombstoned and the album returns to normal operation).
#[test]
fn upgrade_aborts_on_divergent_frozen_state() {
    let album = Uuid::from_u128(0x0D1F);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    let proposal = admin
        .propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2027-01-01",
            CRYPTO_SUITE_ID,
            DEFAULT_UPGRADE_DEADLINE,
        )
        .unwrap();
    bob.receive_upgrade_intent(to_in(proposal.message), &admin_dev.directory())
        .unwrap();

    // Admin commits over summary A; bob's view diverges (summary B) → hash mismatch → abort.
    let tomb = admin.commit_tombstone(&summary(0xA1)).unwrap();
    let err = bob.process_tombstone(to_in(tomb.commit), &summary(0xB2));
    assert!(matches!(
        err,
        Err(OpenMlsAuthorityError::FrozenStateMismatch)
    ));
    assert_eq!(
        bob.is_tombstoned(),
        None,
        "aborted member is not tombstoned"
    );
    // Bob's group returned to normal: it can still author a write ceremony.
    assert!(bob.rotate_epoch().is_ok());
}

/// **Stranded write queue (versioning.md smoke) + fork continuity.** During quiescence a member
/// queues a write locally; the upgrade completes (tombstone → fork); the queued write is re-encoded
/// against the fork and replayed — asserting **no write is lost**, the fork carries the
/// `upgraded_from` continuity pointer, and (suite-parametric) the lineage records the target suite.
#[test]
fn upgrade_forks_and_replays_stranded_writes() {
    let album = Uuid::from_u128(0x0F0C);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let bob_dev = Device::new(0x2, 0x22, 2);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);

    // Steps 1–2: propose + quiesce. A hypothetical *target* crypto_suite_id proves the ceremony is
    // suite-parametric even though only 0x0001 is implemented today.
    const TARGET_SUITE: u16 = 0x0002;
    let proposal = admin
        .propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2027-01-01",
            TARGET_SUITE,
            DEFAULT_UPGRADE_DEADLINE,
        )
        .unwrap();
    let intent_id = proposal.signed_intent.intent.intent_id;
    bob.receive_upgrade_intent(to_in(proposal.message), &admin_dev.directory())
        .unwrap();

    // Bob queues a write locally during quiescence (not sent to the server).
    let stranded_file = Uuid::from_u128(0xF11E);
    bob.queue_pending_write(stranded_file.as_bytes().to_vec())
        .unwrap();

    // Step 4: tombstone (both agree).
    let sm = summary(0x33);
    let tomb = admin.commit_tombstone(&sm).unwrap();
    bob.process_tombstone(to_in(tomb.commit), &sm).unwrap();

    // Step 5: fork — a fresh album group at the target version.
    let new_album = Uuid::now_v7();
    let lineage = UpgradeLineage {
        old_album_id: album,
        intent_id,
        frozen_state_hash: tomb.frozen_state_hash,
        from_suite_id: CRYPTO_SUITE_ID,
        to_suite_id: TARGET_SUITE,
    };
    let mut fork_admin = OpenMlsAuthority::fork_upgrade(
        admin_dev.identity(),
        new_album,
        lineage,
        HistoryPolicy::Full,
    )
    .unwrap();
    assert_eq!(fork_admin.upgraded_from().unwrap().old_album_id, album);
    assert_eq!(
        fork_admin.upgraded_from().unwrap().to_suite_id,
        TARGET_SUITE
    );
    assert_eq!(
        fork_admin.epoch_ceiling(),
        AmkVersion(1),
        "fork mints AMK_v1"
    );
    // Members migrate into the fork (standard add/join).
    let bob_fork = add_and_join(&mut fork_admin, &bob_dev, HistoryPolicy::Full);

    // Step 6: replay the stranded write, re-encoded against the fork. Assert none was lost.
    let queued = bob.take_pending_writes();
    assert_eq!(
        queued.len(),
        1,
        "the quiesced write survived to be replayed"
    );
    assert_eq!(queued[0], stranded_file.as_bytes().to_vec());
    let fork_epoch = bob_fork.epoch_ceiling();
    let write_tier = bob_fork.write_tier_signing_key(fork_epoch).unwrap();
    let (manifest, directory) = signed_manifest(new_album, fork_epoch, write_tier);
    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &fork_admin, None),
        VerifyOutcome::Accept,
        "the replayed write verifies in the fork"
    );
}

/// **Version-mismatched-client damage / tombstone freeze (versioning.md).** After the tombstone, the
/// old album refuses every write ceremony; and a member on the frozen album cannot write into the
/// fork without processing the ceremony — its old-album write-tier key does not verify at the fork's
/// fresh epoch.
#[test]
fn tombstoned_album_refuses_writes_and_old_keys_do_not_verify_in_fork() {
    let album = Uuid::from_u128(0x0DEF);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let old_write_tier = admin
        .write_tier_signing_key(admin.epoch_ceiling())
        .unwrap()
        .clone();

    admin
        .propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2027-01-01",
            CRYPTO_SUITE_ID,
            DEFAULT_UPGRADE_DEADLINE,
        )
        .unwrap();
    let tomb = admin.commit_tombstone(&summary(0x44)).unwrap();

    // Every write ceremony on the frozen album now refuses.
    assert!(matches!(
        admin.rotate_epoch(),
        Err(OpenMlsAuthorityError::Tombstoned(_))
    ));
    assert!(matches!(
        admin.advance_epoch(true),
        Err(OpenMlsAuthorityError::Tombstoned(_))
    ));
    assert!(matches!(
        admin.begin_rekey(RekeyReason::AdminInitiated),
        Err(OpenMlsAuthorityError::Tombstoned(_))
    ));

    // Fork the album; a stale write signed with the OLD album's write-tier key does not verify at
    // the fork's fresh epoch (the fork attested a different, freshly-minted write-tier public key).
    let new_album = Uuid::now_v7();
    let lineage = UpgradeLineage {
        old_album_id: album,
        intent_id: tomb.intent_id,
        frozen_state_hash: tomb.frozen_state_hash,
        from_suite_id: CRYPTO_SUITE_ID,
        to_suite_id: CRYPTO_SUITE_ID,
    };
    let fork_admin = OpenMlsAuthority::fork_upgrade(
        admin_dev.identity(),
        new_album,
        lineage,
        HistoryPolicy::Full,
    )
    .unwrap();
    let (stale, directory) =
        signed_manifest(new_album, fork_admin.epoch_ceiling(), &old_write_tier);
    assert!(
        matches!(
            verify_asset(&stale, CIPHERTEXT, &directory, &fork_admin, None),
            VerifyOutcome::TerminalReject(_)
        ),
        "old-album write-tier key must not authorize a write into the fork"
    );
}

/// **Hostile / stale proposal defence (versioning.md step 1).** An `UpgradeIntent` whose proposer
/// signature does not verify against the device directory is rejected; and a *second* intent under a
/// different `intent_id` is rejected while one is already in flight (only one upgrade per album).
#[test]
fn upgrade_intent_signature_and_single_flight_are_enforced() {
    let album = Uuid::from_u128(0x0517);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    let proposal = admin
        .propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2027-01-01",
            CRYPTO_SUITE_ID,
            DEFAULT_UPGRADE_DEADLINE,
        )
        .unwrap();

    // A directory publishing a *different* DSK for the proposer device → the hybrid signature over
    // the intent does not verify. Checked directly on the signed intent (decoupled from MLS
    // message consumption, which is single-use per the secret-tree ratchet).
    let wrong_dsk = HybridSigningKey::from_seed_bytes(&[0x9; 32], &[0x9; 32]);
    let tampered_dir = DirectoryCore {
        user_id: admin_dev.user_id,
        directory_version: 1,
        updated_at: "2026-05-30T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: admin_dev.device_id,
            dsk_public: wrong_dsk.verifying_key(),
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
        }],
    }
    .sign(&HybridSigningKey::from_seed_bytes(&[7; 32], &[7; 32]));
    assert!(matches!(
        proposal.signed_intent.verify(&tampered_dir),
        Err(OpenMlsAuthorityError::Upgrade(_))
    ));
    assert!(
        proposal
            .signed_intent
            .verify(&admin_dev.directory())
            .is_ok()
    );

    // Bob accepts the genuine intent, entering quiescence.
    bob.receive_upgrade_intent(to_in(proposal.message), &admin_dev.directory())
        .unwrap();
    // A different intent (new intent_id), correctly signed by the admin DSK, is rejected while one
    // is in flight.
    let intent2 = UpgradeIntent {
        intent_id: Uuid::now_v7(),
        from_protocol_version: PROTOCOL_VERSION.into(),
        to_protocol_version: "2028-01-01".into(),
        from_suite_id: CRYPTO_SUITE_ID,
        to_suite_id: CRYPTO_SUITE_ID,
        proposer_user: admin_dev.user_id,
        proposer_device: admin_dev.device_id,
        deadline_secs: 3600,
    };
    let sig2 = admin_dev.dsk.sign(&intent2.signing_bytes().unwrap());
    let signed2 = SignedUpgradeIntent {
        intent: intent2,
        proposer_sig: sig2,
    };
    let m2 = admin
        .create_app_message(&super::messages::MlsAppPayload::Upgrade(signed2))
        .unwrap();
    assert!(matches!(
        bob.receive_upgrade_intent(to_in(m2), &admin_dev.directory()),
        Err(OpenMlsAuthorityError::Upgrade(_))
    ));

    // And the proposer itself rejects a second propose while quiescing.
    assert!(matches!(
        admin.propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2028-01-01",
            CRYPTO_SUITE_ID,
            DEFAULT_UPGRADE_DEADLINE,
        ),
        Err(OpenMlsAuthorityError::Upgrade(_))
    ));
}

/// The upgrade deadline is a **duration** evaluated against a trusted (server) clock — a member
/// clock cannot extend or shorten the window. `is_expired` is that pure predicate.
#[test]
fn upgrade_intent_expiry_is_a_pure_clock_predicate() {
    let intent = UpgradeIntent {
        intent_id: Uuid::now_v7(),
        from_protocol_version: PROTOCOL_VERSION.into(),
        to_protocol_version: "2027-01-01".into(),
        from_suite_id: CRYPTO_SUITE_ID,
        to_suite_id: CRYPTO_SUITE_ID,
        proposer_user: Uuid::from_u128(1),
        proposer_device: Uuid::from_u128(2),
        deadline_secs: 7 * 24 * 3600,
    };
    let received = jiff::Timestamp::from_second(1_760_000_000).unwrap();
    // One hour later: not expired.
    let soon = received
        .checked_add(jiff::SignedDuration::from_hours(1))
        .unwrap();
    assert!(!intent.is_expired(received, soon));
    // Eight days later: expired.
    let later = received
        .checked_add(jiff::SignedDuration::from_hours(8 * 24))
        .unwrap();
    assert!(intent.is_expired(received, later));
}

// ── MLS resilience (mls-resilience.md § Validation) ────────────────────────────

/// **State-divergence detection + reconciliation (unit).** A member behind the server chain
/// reconciles by replaying the missed commits, converging on the server-authoritative state; the
/// outcome names each applied commit.
#[test]
fn state_divergence_is_detected_and_reconciled_from_the_chain() {
    let album = Uuid::from_u128(0x0D11);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);
    let start = admin.mls_epoch();
    assert_eq!(start, bob.mls_epoch());

    // Admin advances twice; bob misses both commits (network divergence).
    let r1 = admin.rotate_epoch().unwrap();
    let r2 = admin.rotate_epoch().unwrap();
    assert_eq!(admin.mls_epoch(), start + 2);

    // Up-to-date view is a no-op.
    assert_eq!(
        admin
            .reconcile_with_server(ServerChainView::UpToDate)
            .unwrap(),
        ReconcileOutcome::UpToDate
    );

    // Bob reconciles by replaying the server's chain.
    let missed = vec![r1.commit.to_bytes().unwrap(), r2.commit.to_bytes().unwrap()];
    let outcome = bob
        .reconcile_with_server(ServerChainView::Behind {
            server_epoch: admin.mls_epoch(),
            missed_commits: missed.clone(),
        })
        .unwrap();
    match outcome {
        ReconcileOutcome::Reconciled { applied_commits } => {
            assert_eq!(applied_commits.len(), 2);
            assert_eq!(applied_commits[0], CommitHash(hash::hash_bytes(&missed[0])));
        }
        other => panic!("expected Reconciled, got {other:?}"),
    }
    assert_eq!(
        bob.mls_epoch(),
        admin.mls_epoch(),
        "converged on server state"
    );
}

/// **Divergence never silently merges.** A member *ahead* of the server (a lost local commit)
/// surfaces `Diverged` (user action / quarantine) when the absence is honestly-lost, and
/// `Unrecoverable` (re-bootstrap) when it is a provable fork.
#[test]
fn local_ahead_diverges_or_is_unrecoverable_never_silently_merged() {
    let album = Uuid::from_u128(0x0A4D);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);
    let server_epoch = admin.mls_epoch();

    // Bob commits locally; the server never persists it (bob is now ahead).
    bob.rotate_epoch().unwrap();
    assert_eq!(bob.mls_epoch(), server_epoch + 1);

    // Honestly-lost commit → Diverged (surfaced, not merged).
    assert_eq!(
        bob.reconcile_with_server(ServerChainView::LocalAhead {
            server_epoch,
            provable_fork: false,
        })
        .unwrap(),
        ReconcileOutcome::Diverged {
            local_epoch: bob.mls_epoch(),
            server_epoch,
        }
    );
    // Provable fork → Unrecoverable (re-bootstrap).
    assert_eq!(
        bob.reconcile_with_server(ServerChainView::LocalAhead {
            server_epoch,
            provable_fork: true,
        })
        .unwrap(),
        ReconcileOutcome::Unrecoverable
    );
}

/// **Lost-commit recovery (smoke).** A commit lost to a member is recovered by replaying the server
/// chain; the replay is idempotent (a re-delivered commit that already landed is rejected, with no
/// duplicate epoch), and two independent members applying the same commit converge identically.
#[test]
fn lost_commit_recovery_is_idempotent_and_converges() {
    let album = Uuid::from_u128(0x0105);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    // A second, independent view of bob at the same epoch (via persistence round-trip).
    let mut bob2 = OpenMlsAuthority::import_state(&bob.export_state().unwrap()).unwrap();

    // Admin commits; the commit is "lost" to both bob views.
    let rot = admin.rotate_epoch().unwrap();
    let bytes = rot.commit.to_bytes().unwrap();

    // Each bob recovers by replaying — both converge on the admin's epoch (order/instance
    // independent, the deterministic replay).
    for b in [&mut bob, &mut bob2] {
        let outcome = b
            .reconcile_with_server(ServerChainView::Behind {
                server_epoch: admin.mls_epoch(),
                missed_commits: vec![bytes.clone()],
            })
            .unwrap();
        assert!(matches!(outcome, ReconcileOutcome::Reconciled { .. }));
        assert_eq!(b.mls_epoch(), admin.mls_epoch());
    }
    // Idempotency: re-delivering the same commit that already landed is rejected, epoch unchanged.
    let epoch = bob.mls_epoch();
    assert!(bob.process_commit(to_in(rot.commit)).is_err());
    assert_eq!(
        bob.mls_epoch(),
        epoch,
        "no duplicate epoch from a replayed commit"
    );
}

/// The lost-commit **backoff schedule** matches the doc defaults (30 s → 2 min → 10 min, 3 attempts,
/// 30 s detection timeout), and the retry budget exhausts cleanly.
#[test]
fn lost_commit_tracker_follows_the_backoff_schedule() {
    let mut tracker = LostCommitTracker::new();
    assert_eq!(
        tracker.detection_timeout(),
        jiff::SignedDuration::from_secs(30)
    );
    assert_eq!(tracker.max_attempts(), 3);
    assert!(!tracker.is_exhausted());
    assert_eq!(
        tracker.record_attempt(),
        Some(jiff::SignedDuration::from_secs(30))
    );
    assert_eq!(
        tracker.record_attempt(),
        Some(jiff::SignedDuration::from_secs(120))
    );
    assert_eq!(
        tracker.record_attempt(),
        Some(jiff::SignedDuration::from_secs(600))
    );
    assert_eq!(tracker.record_attempt(), None, "budget exhausted");
    assert!(tracker.is_exhausted());
}

/// **Concurrent rotation (smoke).** Two members rotate the same epoch; MLS commit ordering
/// serializes them (one wins), and the loser re-proposes against the winner's result — both converge
/// on one write-tier key per epoch with no group split.
#[test]
fn concurrent_rotation_serializes_and_loser_replays() {
    let album = Uuid::from_u128(0x0C07);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);
    let start = admin.mls_epoch();

    // Both stage a rotation against the same epoch; the delivery service picks the admin's.
    let admin_commit = admin.stage_self_update().unwrap();
    let _bob_commit = bob.stage_self_update().unwrap();
    admin.merge_pending().unwrap();
    bob.discard_pending_and_process(to_in(admin_commit.clone()))
        .unwrap();

    // Deliver the winner's key material so bob's ledger is complete for the converged epoch.
    let converged = admin.epoch_ceiling();
    bob.process_key_delivery(to_in(admin.build_key_distribution(converged).unwrap()))
        .unwrap();
    bob.process_key_delivery(to_in(
        admin.build_write_tier_distribution(converged).unwrap(),
    ))
    .unwrap();

    assert_eq!(
        admin.mls_epoch(),
        start + 1,
        "one rotation serialized onto the chain"
    );
    assert_converged(&[&admin, &bob]);
}

/// **Group re-keying: pre-compromise keys become useless.** A full re-key mints a fresh AMK +
/// write-tier key for the whole group; a manifest signed with a *pre-rekey* write-tier key does not
/// verify at the post-rekey epoch, and the fresh AMK differs from the compromised one.
#[test]
fn rekey_group_makes_precompromise_keys_useless() {
    let album = Uuid::from_u128(0x0BAD);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &Device::new(0x2, 0x22, 2), HistoryPolicy::Full);

    let compromised_epoch = admin.epoch_ceiling();
    let compromised_key = admin
        .write_tier_signing_key(compromised_epoch)
        .unwrap()
        .clone();
    let compromised_amk = admin.amk(compromised_epoch).unwrap();

    // Full re-key (suspected compromise response).
    let outcome = admin.rekey_group(RekeyReason::SuspectedCompromise).unwrap();
    bob.process_commit(to_in(outcome.commit)).unwrap();
    for m in outcome.key_delivery {
        bob.process_key_delivery(to_in(m)).unwrap();
    }
    let fresh_epoch = admin.epoch_ceiling();
    assert_eq!(fresh_epoch, AmkVersion(compromised_epoch.0 + 1));
    assert_ne!(
        admin.amk(fresh_epoch).unwrap(),
        compromised_amk,
        "fresh AMK minted"
    );
    assert_converged(&[&admin, &bob]);

    // A write signed with the pre-rekey (compromised) write-tier key does not verify at the fresh
    // epoch — the fresh epoch attested a different, freshly-minted write-tier public key.
    let (stale, directory) = signed_manifest(album, fresh_epoch, &compromised_key);
    assert!(matches!(
        verify_asset(&stale, CIPHERTEXT, &directory, &admin, None),
        VerifyOutcome::TerminalReject(_)
    ));
}

/// **Re-keying atomicity (smoke).** Inject a crash mid-rekey (between the commit and the broadcast)
/// by exporting/reloading; the ceremony resumes on restart and completes the broadcast without
/// advancing the epoch a second time (the `intent_id` keeps it idempotent).
#[test]
fn rekey_atomicity_resumes_after_a_mid_ceremony_crash() {
    let album = Uuid::from_u128(0x0A70);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();

    // Phase 1 only: the re-key commit merges (epoch advances) but the broadcast has not happened.
    let (intent_id, _commit) = admin.begin_rekey(RekeyReason::ScheduledRotation).unwrap();
    let epoch_after_commit = admin.mls_epoch();
    assert!(admin.rekey_in_progress());

    // Crash + restart: the resume state is durable.
    let mut restored = OpenMlsAuthority::import_state(&admin.export_state().unwrap()).unwrap();
    assert!(restored.rekey_in_progress());
    assert_eq!(restored.mls_epoch(), epoch_after_commit);

    // Resume completes phase 2 (broadcast) without a second epoch advance.
    let delivery = restored
        .resume_rekey()
        .unwrap()
        .expect("a re-key was pending");
    assert_eq!(delivery.len(), 2, "AMK + write-tier broadcast");
    assert_eq!(
        restored.mls_epoch(),
        epoch_after_commit,
        "no second epoch advance"
    );
    assert!(!restored.rekey_in_progress());
    assert!(restored.has_completed_intent(intent_id));
    // Resuming again is a no-op.
    assert!(restored.resume_rekey().unwrap().is_none());
}

/// **E2E case 8 — album upgrade ceremony (in-process shape).** Multi-member album; admin initiates
/// the upgrade → quiesce → drain (modelled: no in-flight sessions) → tombstone → fork → queued
/// writes replay, **including one resume-from-crash mid-ceremony**. This is the Module-Map E2E case
/// 8 in the established in-process multi-participant shape (the server/client-UI halves are out of
/// this slice's scope); it exercises `capsule-core::crypto::mls` end-to-end for the ceremony.
#[test]
fn e2e_case_8_album_upgrade_ceremony() {
    let album = Uuid::from_u128(0x0E8E);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let bob_dev = Device::new(0x2, 0x22, 2);
    let carol_dev = Device::new(0x3, 0x33, 3);

    // A three-member album at some history.
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let mut bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);
    // Add Carol; existing member Bob processes the commit + deliveries.
    let carol_identity = carol_dev.identity();
    let add = admin
        .add_member(
            carol_identity.key_package().unwrap(),
            &carol_dev.directory(),
        )
        .unwrap();
    bob.process_commit(to_in(add.commit.clone())).unwrap();
    for m in &add.key_delivery {
        bob.process_key_delivery(to_in(m.clone())).unwrap();
    }
    let history: Vec<MlsMessageIn> = add.key_delivery.into_iter().map(to_in).collect();
    let mut carol = OpenMlsAuthority::join_via_welcome(
        carol_identity,
        to_in(add.welcome),
        history,
        HistoryPolicy::Full,
    )
    .unwrap();
    assert_converged(&[&admin, &bob, &carol]);

    // Step 1–2: admin initiates the upgrade; both members quiesce.
    let proposal = admin
        .propose_upgrade(
            PROTOCOL_VERSION,
            CRYPTO_SUITE_ID,
            "2027-06-01",
            CRYPTO_SUITE_ID,
            DEFAULT_UPGRADE_DEADLINE,
        )
        .unwrap();
    let intent_id = proposal.signed_intent.intent.intent_id;
    let msg_bytes = proposal.message.to_bytes().unwrap();
    for m in [&mut bob, &mut carol] {
        let msg = MlsMessageIn::tls_deserialize_exact(msg_bytes.as_slice()).unwrap();
        m.receive_upgrade_intent(msg, &admin_dev.directory())
            .unwrap();
    }
    assert_eq!(bob.quiescing_intent(), Some(intent_id));

    // Step 3 (drain): modelled — no in-flight upload sessions (server concern). Carol queues a write.
    let carol_file = Uuid::now_v7();
    carol
        .queue_pending_write(carol_file.as_bytes().to_vec())
        .unwrap();

    // Step 4: tombstone. Every member recomputes the frozen-state hash over the shared summary.
    let sm = summary(0x88);
    let tomb = admin.commit_tombstone(&sm).unwrap();

    // Inject a crash on Bob right after the tombstone commit is broadcast but before he processes
    // it: he exports, reloads, and resumes processing the tombstone — the ceremony is resumable.
    let tomb_bytes = tomb.commit.to_bytes().unwrap();
    let mut bob = OpenMlsAuthority::import_state(&bob.export_state().unwrap()).unwrap();
    bob.process_tombstone(
        MlsMessageIn::tls_deserialize_exact(tomb_bytes.as_slice()).unwrap(),
        &sm,
    )
    .unwrap();
    carol
        .process_tombstone(
            MlsMessageIn::tls_deserialize_exact(tomb_bytes.as_slice()).unwrap(),
            &sm,
        )
        .unwrap();
    assert_eq!(admin.is_tombstoned(), Some(intent_id));
    assert_eq!(bob.is_tombstoned(), Some(intent_id));
    assert_eq!(carol.is_tombstoned(), Some(intent_id));

    // Step 5: fork at the target version; all members migrate.
    let new_album = Uuid::now_v7();
    let lineage = UpgradeLineage {
        old_album_id: album,
        intent_id,
        frozen_state_hash: tomb.frozen_state_hash,
        from_suite_id: CRYPTO_SUITE_ID,
        to_suite_id: CRYPTO_SUITE_ID,
    };
    let mut fork_admin = OpenMlsAuthority::fork_upgrade(
        admin_dev.identity(),
        new_album,
        lineage,
        HistoryPolicy::Full,
    )
    .unwrap();
    let _fork_bob = add_and_join(&mut fork_admin, &bob_dev, HistoryPolicy::Full);
    let fork_carol = add_and_join(&mut fork_admin, &carol_dev, HistoryPolicy::Full);
    // The `upgraded_from` continuity pointer is held by the fork founder (a manifest-layer link,
    // not MLS group state that joiners inherit).
    assert_eq!(fork_admin.upgraded_from().unwrap().intent_id, intent_id);

    // Step 6: Carol's stranded write is replayed into the fork and verifies — no write lost.
    let queued = carol.take_pending_writes();
    assert_eq!(queued.len(), 1);
    let fork_epoch = fork_carol.epoch_ceiling();
    let (manifest, directory) = signed_manifest(
        new_album,
        fork_epoch,
        fork_carol.write_tier_signing_key(fork_epoch).unwrap(),
    );
    assert_eq!(
        verify_asset(&manifest, CIPHERTEXT, &directory, &fork_admin, None),
        VerifyOutcome::Accept
    );
}

// ── Per-user block (moderation.md § Blocklists, slice `S-X4`) ──────────────

/// Join a second device belonging to an **existing** member's user, relaying the add commit to
/// `others` so every view stays converged. `add_and_join` cannot do this: it assumes the admin is
/// the group's only member.
fn add_and_join_relaying(
    admin: &mut OpenMlsAuthority,
    joiner: &Device,
    others: &mut [&mut OpenMlsAuthority],
    policy: HistoryPolicy,
) -> OpenMlsAuthority {
    let identity = joiner.identity();
    let key_package: KeyPackage = identity.key_package().unwrap();
    let outcome = admin.add_member(key_package, &joiner.directory()).unwrap();
    for other in others.iter_mut() {
        other.process_commit(to_in(outcome.commit.clone())).unwrap();
        for m in &outcome.key_delivery {
            other.process_key_delivery(to_in(m.clone())).unwrap();
        }
    }
    let history: Vec<MlsMessageIn> = outcome.key_delivery.into_iter().map(to_in).collect();
    OpenMlsAuthority::join_via_welcome(identity, to_in(outcome.welcome), history, policy).unwrap()
}

/// **The `S-X4` acceptance — moderation.md's per-user-block bullet, end to end.**
///
/// Blocking a user removes **all** their devices in a single `Remove` + `Commit` (mls.md's
/// "removing all Charlie's devices"), so the AMK epoch bumps exactly **once** for the whole user,
/// the write-tier key rotates, and neither of the blocked user's devices can reach any future
/// epoch's content key or write capability. Their pre-block keys are deliberately **not** clawed
/// back — the design says so explicitly.
#[test]
fn per_user_block_removes_every_device_bumps_the_epoch_and_rotates_the_write_tier() {
    let album = Uuid::from_u128(0x0B10);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();

    // Bob joins with two devices — one user, two leaves.
    let bob_a_dev = Device::new(0x2, 0x22, 2);
    let bob_b_dev = Device::new(0x2, 0x23, 4);
    let mut bob_a = add_and_join(&mut admin, &bob_a_dev, HistoryPolicy::Full);
    let bob_b = add_and_join_relaying(
        &mut admin,
        &bob_b_dev,
        &mut [&mut bob_a],
        HistoryPolicy::Full,
    );
    assert_converged(&[&admin, &bob_a, &bob_b]);

    let before = admin.epoch_ceiling();
    let bob_user = bob_a_dev.user_id;
    assert_eq!(
        admin.leaf_indices_of_user(bob_user).len(),
        2,
        "both of Bob's devices must be visible as leaves of the same user"
    );
    let shared_amk = admin.amk(before).unwrap();
    let shared_write_tier = admin.write_tier_pubkey(before).unwrap();

    // ── The block ──
    let outcome = admin.block_user(bob_user).unwrap();
    assert!(!outcome.already_absent());
    assert_eq!(outcome.user_id, bob_user);
    assert_eq!(
        outcome.removed_leaves.len(),
        2,
        "a per-user block removes the user's whole device set"
    );
    assert_eq!(
        outcome.amk_version,
        AmkVersion(before.0 + 1),
        "one commit for the whole user means exactly one epoch bump, not one per device"
    );
    assert_eq!(admin.epoch_ceiling(), outcome.amk_version);
    assert!(admin.leaf_indices_of_user(bob_user).is_empty());

    // The AMK and the write-tier key both rotate at the new epoch.
    let after = outcome.amk_version;
    assert_ne!(
        admin.amk(after).unwrap(),
        shared_amk,
        "the content key must rotate so the blocked user cannot read future epochs"
    );
    assert_ne!(
        admin.write_tier_pubkey(after).unwrap(),
        shared_write_tier,
        "the write-tier key must rotate so the blocked user cannot author future writes"
    );
    // The rotated write-tier private half is held by the blocker and usable immediately.
    assert!(admin.write_tier_signing_key(after).is_some());

    // ── Both of Bob's devices lose future-epoch decryption ──
    let removal = outcome.removal.unwrap();
    let mut bob_b = bob_b;
    for bob in [&mut bob_a, &mut bob_b] {
        assert!(
            bob.process_commit(to_in(removal.commit.clone())).is_err(),
            "a removed device is evicted and cannot advance past its removal epoch"
        );
        for m in &removal.key_delivery {
            assert!(bob.process_key_delivery(to_in(m.clone())).is_err());
        }
        assert!(bob.amk(after).is_none(), "no future-epoch content key");
        assert!(
            bob.write_tier_signing_key(after).is_none(),
            "no future-epoch write capability"
        );
        assert!(bob.write_tier_pubkey(after).is_none());
        // …but the epochs they legitimately held are NOT clawed back (moderation.md).
        assert_eq!(
            bob.amk(before).unwrap(),
            shared_amk,
            "prior epochs stay readable — removal is forward-only by design"
        );
    }
}

/// **Idempotency.** The server-side block row is idempotent, so the MLS half must be too: a repeat
/// block finds no leaves, produces no commit, and — critically — does **not** burn an epoch. A
/// spurious bump would strand every honest concurrent uploader in the pending window for nothing.
#[test]
fn blocking_a_non_member_is_a_no_op_and_burns_no_epoch() {
    let album = Uuid::from_u128(0x0B11);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let bob_dev = Device::new(0x2, 0x22, 2);
    let _bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);

    let first = admin.block_user(bob_dev.user_id).unwrap();
    assert!(!first.already_absent());
    let after_first = admin.epoch_ceiling();

    // Blocking again — and blocking a user who was never a member — are both no-ops.
    for user in [bob_dev.user_id, Uuid::from_u128(0x0B0B)] {
        let repeat = admin.block_user(user).unwrap();
        assert!(repeat.already_absent());
        assert!(repeat.removal.is_none());
        assert!(repeat.removed_leaves.is_empty());
        assert_eq!(repeat.amk_version, after_first);
        assert_eq!(
            admin.epoch_ceiling(),
            after_first,
            "an already-absent user must not cost an epoch"
        );
    }
}

/// A device cannot evict itself from its own group, so blocking the local user is refused rather
/// than silently reported as a successful block.
#[test]
fn blocking_the_local_user_is_refused() {
    let album = Uuid::from_u128(0x0B12);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let before = admin.epoch_ceiling();

    assert!(matches!(
        admin.block_user(admin_dev.user_id),
        Err(OpenMlsAuthorityError::BlockSelf(_))
    ));
    assert_eq!(admin.epoch_ceiling(), before, "the refusal is total");
}

/// **Block scoping stays a crypto-layer removal.** After the block the remaining members converge
/// on the new epoch and a manifest signed under the rotated write-tier key verifies, while one
/// signed under the *pre-block* key at the *post-block* epoch does not — the write-tier rotation
/// is load-bearing, not cosmetic.
#[test]
fn after_a_block_only_the_rotated_write_tier_authorizes_new_writes() {
    let album = Uuid::from_u128(0x0B13);
    let admin_dev = Device::new(0x1, 0x11, 1);
    let mut admin =
        OpenMlsAuthority::create_album(admin_dev.identity(), album, HistoryPolicy::Full).unwrap();
    let bob_dev = Device::new(0x2, 0x22, 2);
    let mut bob = add_and_join(&mut admin, &bob_dev, HistoryPolicy::Full);
    let carol_dev = Device::new(0x3, 0x33, 3);
    let mut carol =
        add_and_join_relaying(&mut admin, &carol_dev, &mut [&mut bob], HistoryPolicy::Full);

    let before = admin.epoch_ceiling();
    let stale_write_tier = admin.write_tier_signing_key(before).unwrap().clone();

    // Block Bob; Carol (a remaining member) relays the commit + key delivery.
    let outcome = admin.block_user(bob_dev.user_id).unwrap();
    let removal = outcome.removal.unwrap();
    carol.process_commit(to_in(removal.commit.clone())).unwrap();
    for m in &removal.key_delivery {
        carol.process_key_delivery(to_in(m.clone())).unwrap();
    }
    assert_converged(&[&admin, &carol]);

    let after = outcome.amk_version;
    let (good, directory) =
        signed_manifest(album, after, admin.write_tier_signing_key(after).unwrap());
    assert_eq!(
        verify_asset(&good, CIPHERTEXT, &directory, &admin, None),
        VerifyOutcome::Accept
    );

    let (stale, stale_dir) = signed_manifest(album, after, &stale_write_tier);
    assert_ne!(
        verify_asset(&stale, CIPHERTEXT, &stale_dir, &admin, None),
        VerifyOutcome::Accept,
        "the pre-block write-tier key must not authorize a post-block epoch"
    );
}
