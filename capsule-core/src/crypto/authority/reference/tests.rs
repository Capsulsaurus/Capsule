use super::*;

fn setup() -> (Uuid, HybridSigningKey, HybridSigningKey, HybridSigningKey) {
    // album, admin key, write-tier epoch 1 key, write-tier epoch 2 key
    (
        Uuid::from_u128(0xA1),
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]),
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]),
        HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]),
    )
}

#[test]
fn lookups_reflect_attested_epochs() {
    let (album, admin, w1, w2) = setup();
    let auth = ReferenceAuthority::new(album, admin.verifying_key())
        .with_epoch(&admin, AmkVersion(1), &w1.verifying_key(), true)
        .with_epoch(&admin, AmkVersion(2), &w2.verifying_key(), false);

    assert!(auth.admin_chain_verifies());
    assert_eq!(auth.epoch_ceiling(), AmkVersion(2));
    assert_eq!(
        auth.write_tier_pubkey(AmkVersion(1)),
        Some(w1.verifying_key())
    );
    assert_eq!(
        auth.write_tier_pubkey(AmkVersion(2)),
        Some(w2.verifying_key())
    );
    assert_eq!(auth.write_tier_pubkey(AmkVersion(3)), None);
    // Epoch 1 AMK held; epoch 2 not yet (pending territory).
    assert!(auth.has_amk(AmkVersion(1)));
    assert!(!auth.has_amk(AmkVersion(2)));
}

#[test]
fn mark_amk_present_flips_pending() {
    let (album, admin, w1, _) = setup();
    let mut auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
        &admin,
        AmkVersion(1),
        &w1.verifying_key(),
        false,
    );
    assert!(!auth.has_amk(AmkVersion(1)));
    auth.mark_amk_present(AmkVersion(1));
    assert!(auth.has_amk(AmkVersion(1)));
}

#[test]
fn forged_attestation_fails_admin_chain() {
    let (album, admin, w1, _) = setup();
    let mut auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
        &admin,
        AmkVersion(1),
        &w1.verifying_key(),
        true,
    );
    // Tamper an entry's signature: the admin chain must no longer verify.
    auth.entries.get_mut(&1).unwrap().admin_sig =
        HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]).sign(b"not the attestation");
    assert!(!auth.admin_chain_verifies());
}

#[test]
fn attestation_signed_by_wrong_admin_fails() {
    let (album, admin, w1, _) = setup();
    let imposter = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
    // Build with the imposter signing, but claim the real admin's public key.
    let mut auth = ReferenceAuthority::new(album, admin.verifying_key());
    // Bypass the debug_assert by inserting a wrongly-signed entry directly.
    let wt = w1.verifying_key();
    let sig = imposter.sign(&attestation_bytes(album, AmkVersion(1), &wt));
    auth.entries.insert(
        1,
        Entry {
            write_tier_pub: wt,
            admin_sig: sig,
            amk_present: true,
        },
    );
    auth.ceiling = AmkVersion(1);
    assert!(
        !auth.admin_chain_verifies(),
        "an attestation not signed by the declared admin must be rejected"
    );
}

#[test]
fn ceiling_inconsistent_with_entries_fails() {
    let (album, admin, w1, _) = setup();
    let mut auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
        &admin,
        AmkVersion(1),
        &w1.verifying_key(),
        true,
    );
    // Fabricate a higher ceiling than any attested epoch.
    auth.ceiling = AmkVersion(5);
    assert!(!auth.admin_chain_verifies());
}

#[test]
fn ledger_round_trip_reverifies_and_restores_amk_presence() {
    let (album, admin, w1, w2) = setup();
    let auth = ReferenceAuthority::new(album, admin.verifying_key())
        .with_epoch(&admin, AmkVersion(1), &w1.verifying_key(), true)
        .with_epoch(&admin, AmkVersion(2), &w2.verifying_key(), false);

    // Serialize → canonical CBOR → deserialize preserves the ledger.
    let bytes = cbor::to_canonical_vec(&auth.to_ledger()).unwrap();
    let decoded: SignedEpochLedger = cbor::from_slice(&bytes).unwrap();

    // Rebuild holding only epoch 1's AMK locally (presence is restored out-of-band).
    let restored = ReferenceAuthority::from_ledger(&decoded, &BTreeSet::from([1])).unwrap();
    assert!(restored.admin_chain_verifies());
    assert_eq!(restored.epoch_ceiling(), AmkVersion(2));
    assert_eq!(
        restored.write_tier_pubkey(AmkVersion(2)),
        Some(w2.verifying_key())
    );
    assert!(restored.has_amk(AmkVersion(1)));
    assert!(!restored.has_amk(AmkVersion(2)));
}

#[test]
fn tampered_ledger_is_rejected_on_reload() {
    let (album, admin, w1, w2) = setup();
    let auth = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
        &admin,
        AmkVersion(1),
        &w1.verifying_key(),
        true,
    );
    // Swap in a different write-tier key with no matching admin re-signature.
    let mut ledger = auth.to_ledger();
    ledger.entries[0].write_tier_pub = w2.verifying_key();
    assert!(ReferenceAuthority::from_ledger(&ledger, &BTreeSet::from([1])).is_none());
}
