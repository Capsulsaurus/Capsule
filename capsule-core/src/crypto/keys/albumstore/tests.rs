use super::*;
use crate::crypto::keys::AmkVersion;

fn signing_key(a: u8, b: u8) -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[a; 32], &[b; 32])
}

/// A store holding one fully-capable album (keys + attested reference authority) and one
/// read-only recovered album.
fn fixture() -> (AlbumStore, Uuid, Uuid) {
    let writable = Uuid::from_u128(0xAA);
    let recovered = Uuid::from_u128(0x11);

    let admin = signing_key(1, 2);
    let write_tier = signing_key(3, 4);
    let authority = ReferenceAuthority::new(writable, admin.verifying_key()).with_epoch(
        &admin,
        AmkVersion(1),
        &write_tier.verifying_key(),
        true,
    );

    let mut store = AlbumStore::new();
    store.upsert(PersistedAlbum {
        album_id: writable,
        name: "Trip".into(),
        amks: vec![(writable, 1, [7; 32])],
        current_epoch: 1,
        write_tier_seed64: Some(ByteBuf::from(write_tier.to_seed_bytes().to_vec())),
        admin_seed64: Some(ByteBuf::from(admin.to_seed_bytes().to_vec())),
        authority: Some(
            PersistedAuthority::capture(&Authority::Reference(Box::new(authority))).unwrap(),
        ),
    });
    store.upsert(PersistedAlbum::recovered(
        recovered,
        "Restored",
        vec![(recovered, 2, [9; 32]), (recovered, 1, [8; 32])],
        None,
    ));
    (store, writable, recovered)
}

#[test]
fn album_store_round_trips_under_master_key() {
    let master = MasterKey::generate();
    let (store, writable, recovered) = fixture();
    let sealed = store.seal(&master).unwrap();

    // The sealed bytes are not the plaintext.
    assert!(
        !sealed.windows(32).any(|w| w == [7u8; 32]),
        "a raw AMK must never appear in the sealed file"
    );

    let back = AlbumStore::open_sealed(&sealed, &master).unwrap();
    assert_eq!(back.version, ALBUM_STORE_VERSION);
    assert_eq!(back.albums().len(), 2);

    // The writable album keeps its AMK, its signing keys, and a re-verifying authority.
    let a = back.get(&writable).unwrap();
    assert_eq!(a.amks, vec![(writable, 1, [7; 32])]);
    assert_eq!(a.current_epoch, 1);
    assert_eq!(
        a.write_tier_key().unwrap().unwrap().verifying_key(),
        signing_key(3, 4).verifying_key()
    );
    assert_eq!(
        a.admin_key().unwrap().unwrap().verifying_key(),
        signing_key(1, 2).verifying_key()
    );
    let authority = a
        .authority
        .as_ref()
        .unwrap()
        .restore(writable, &a.held_epochs())
        .unwrap();
    assert!(authority.admin_chain_verifies());
    assert_eq!(authority.epoch_ceiling(), AmkVersion(1));
    assert!(
        authority.has_amk(AmkVersion(1)),
        "amk_present is restored from the epochs actually held"
    );

    // The recovered album is honest about holding no write capability.
    let r = back.get(&recovered).unwrap();
    assert_eq!(r.current_epoch, 2, "current epoch is the max recovered one");
    assert_eq!(
        r.amks,
        vec![(recovered, 1, [8; 32]), (recovered, 2, [9; 32])],
        "rows are normalized into epoch order"
    );
    assert!(r.write_tier_key().unwrap().is_none());
    assert!(r.admin_key().unwrap().is_none());
    assert!(r.authority.is_none());
}

#[test]
fn album_store_rejects_wrong_master_key() {
    let master = MasterKey::generate();
    let mut bytes = fixture().0.seal(&master).unwrap();

    let err = AlbumStore::open_sealed(&bytes, &MasterKey::generate()).unwrap_err();
    assert!(
        matches!(err, AlbumStoreError::Crypto(CryptoError::Auth(_))),
        "wrong master key must authenticate-fail, got {err:?}"
    );

    // Tampering with a sealed byte is equally rejected (AES-GCM tag).
    *bytes.last_mut().unwrap() ^= 0x01;
    assert!(AlbumStore::open_sealed(&bytes, &master).is_err());
}

/// The sealed bytes differ every write (fresh nonce — the whole reason this does not reuse
/// the backup artifact's derived-nonce sealer), but the **plaintext** is byte-stable for a
/// given logical state regardless of the order albums or epochs were inserted in.
#[test]
fn album_store_is_canonical_cbor_stable() {
    let (ordered, writable, recovered) = fixture();

    // Build the same logical store with the albums inserted in the opposite order and each
    // album's epochs shuffled.
    let mut shuffled = AlbumStore::new();
    shuffled.upsert(PersistedAlbum::recovered(
        recovered,
        "Restored",
        vec![(recovered, 1, [8; 32]), (recovered, 2, [9; 32])],
        None,
    ));
    for album in ordered.albums() {
        if album.album_id == writable {
            shuffled.upsert(album.clone());
        }
    }

    let encode = |s: &AlbumStore| {
        let mut c = s.clone();
        c.albums.sort_unstable_by_key(|a| a.album_id);
        for a in &mut c.albums {
            a.normalize();
        }
        cbor::to_canonical_vec(&c).unwrap()
    };
    assert_eq!(
        encode(&ordered),
        encode(&shuffled),
        "insertion order must not change the canonical plaintext"
    );

    // And the sealed bytes *are* fresh-nonced: two seals of identical state differ.
    let master = MasterKey::generate();
    let first = ordered.seal(&master).unwrap();
    let second = ordered.seal(&master).unwrap();
    assert_ne!(
        first, second,
        "the store is rewritten on every album mutation, so the nonce MUST be fresh per seal"
    );
}

#[test]
fn merge_amks_adds_missing_epochs_and_never_overwrites_a_held_one() {
    let (mut store, writable, _) = fixture();

    // Epoch 1 is already held with a different value; epoch 2 is new.
    let added = store.merge_amks(
        writable,
        "Trip",
        vec![(writable, 1, [0xEE; 32]), (writable, 2, [0xAB; 32])],
        None,
    );
    assert_eq!(added, 1, "only the unheld epoch is added");
    let a = store.get(&writable).unwrap();
    assert_eq!(
        a.amks,
        vec![(writable, 1, [7; 32]), (writable, 2, [0xAB; 32])],
        "the locally held epoch-1 key is not swapped out from underneath the authority"
    );
    assert_eq!(a.current_epoch, 2);
    // The album keeps its write capability across the merge.
    assert!(a.write_tier_key().unwrap().is_some());

    // An unknown album lands as a read-only recovered entry.
    let fresh = Uuid::from_u128(0xFF);
    assert_eq!(
        store.merge_amks(fresh, "Restored", vec![(fresh, 5, [1; 32])], None),
        1
    );
    let f = store.get(&fresh).unwrap();
    assert!(f.write_tier_key().unwrap().is_none());
    assert_eq!(f.current_epoch, 5);
}

/// #468: a backup's public authority is adopted by an album that holds none — and only by
/// such an album. A held authority is never swapped out, and adopting one grants no signing
/// capability.
#[test]
fn merge_amks_adopts_an_authority_only_where_none_is_held() {
    let (mut store, writable, recovered) = fixture();
    let other_admin = signing_key(5, 6);
    let foreign = |album| {
        PersistedAuthority::Reference(Box::new(
            ReferenceAuthority::new(album, other_admin.verifying_key())
                .with_epoch(
                    &other_admin,
                    AmkVersion(1),
                    &signing_key(7, 8).verifying_key(),
                    true,
                )
                .to_ledger(),
        ))
    };
    let admin_of = |store: &AlbumStore, album: Uuid| {
        let a = store.get(&album).unwrap();
        match a.authority.as_ref().unwrap() {
            PersistedAuthority::Reference(l) => l.admin_pub.clone(),
            PersistedAuthority::OpenMls(_) => panic!("reference fixture"),
        }
    };

    // A held authority is kept.
    store.merge_amks(writable, "Trip", vec![], Some(foreign(writable)));
    assert_eq!(
        admin_of(&store, writable),
        signing_key(1, 2).verifying_key()
    );

    // A recovered album that had none adopts it, and stays read-only.
    store.merge_amks(recovered, "Restored", vec![], Some(foreign(recovered)));
    assert_eq!(admin_of(&store, recovered), other_admin.verifying_key());
    let r = store.get(&recovered).unwrap();
    assert!(r.write_tier_key().unwrap().is_none());
    assert!(r.admin_key().unwrap().is_none());

    // An unknown album lands with it, and it re-verifies on restore.
    let fresh = Uuid::from_u128(0xFF);
    store.merge_amks(
        fresh,
        "Restored",
        vec![(fresh, 1, [1; 32])],
        Some(foreign(fresh)),
    );
    let f = store.get(&fresh).unwrap();
    let authority = f
        .authority
        .as_ref()
        .unwrap()
        .restore(fresh, &f.held_epochs())
        .unwrap();
    assert!(authority.admin_chain_verifies());
    assert!(authority.has_amk(AmkVersion(1)));
}

#[test]
fn a_tampered_reference_ledger_is_rejected_on_restore() {
    let (store, writable, _) = fixture();
    let album = store.get(&writable).unwrap();
    let PersistedAuthority::Reference(ledger) = album.authority.as_ref().unwrap() else {
        panic!("fixture builds a reference authority");
    };
    // Swap the attested write-tier key with no matching admin re-signature.
    let mut forged = ledger.as_ref().clone();
    forged.entries[0].write_tier_pub = signing_key(9, 9).verifying_key();
    match PersistedAuthority::Reference(Box::new(forged)).restore(writable, &album.held_epochs()) {
        Err(AlbumStoreError::LedgerRejected(id)) => assert_eq!(id, writable),
        other => panic!("a forged ledger must be rejected, got {:?}", other.err()),
    }
}

/// A build without `mls` must *decode* an MLS-authority album and report it as a typed
/// error — never panic, and never fail the whole store's decode as if it were corrupt.
#[cfg(not(feature = "mls"))]
#[test]
fn mls_authority_state_on_a_non_mls_build_is_a_typed_error() {
    let master = MasterKey::generate();
    let album_id = Uuid::from_u128(0xC0FFEE);

    let mut store = AlbumStore::new();
    store.upsert(PersistedAlbum::recovered(
        album_id,
        "MLS album",
        vec![(album_id, 1, [3; 32])],
        Some(PersistedAuthority::OpenMls(ByteBuf::from(vec![1, 2, 3, 4]))),
    ));
    let sealed = store.seal(&master).unwrap();

    // The store still loads — the album is present, only its authority is unusable here.
    let back = AlbumStore::open_sealed(&sealed, &master).unwrap();
    let album = back.get(&album_id).unwrap();
    match album
        .authority
        .as_ref()
        .unwrap()
        .restore(album_id, &album.held_epochs())
    {
        Err(AlbumStoreError::MlsUnavailable(id)) => assert_eq!(id, album_id),
        other => panic!(
            "an mls authority on a non-mls build must be typed, got {:?}",
            other.err()
        ),
    }
}

#[test]
fn a_store_from_a_newer_format_version_is_refused_not_misread() {
    let master = MasterKey::generate();
    let mut store = AlbumStore::new();
    store.version = ALBUM_STORE_VERSION + 1;
    // `seal` normalizes the version, so seal the bytes by hand.
    let plaintext = cbor::to_canonical_vec(&store).unwrap();

    assert!(matches!(
        AlbumStore::open_sealed(&master.seal(&plaintext), &master).unwrap_err(),
        AlbumStoreError::UnsupportedVersion(v) if v == ALBUM_STORE_VERSION + 1
    ));
}

/// An album's escrow opens under the master key to the whole album: every epoch, both
/// signing seeds and an authority that re-verifies.
#[test]
fn an_album_key_escrow_round_trips_the_whole_album_under_the_master_key() {
    let master = MasterKey::generate();
    let (store, writable, _) = fixture();
    let album = store.get(&writable).unwrap();
    let blob = album.seal_escrow(&master).unwrap();
    assert!(
        !blob.windows(32).any(|w| w == [7u8; 32]),
        "a raw AMK must never appear in an escrow"
    );

    let back = PersistedAlbum::open_escrow(&blob, &master, writable).unwrap();
    assert_eq!(back.amks, album.amks);
    assert_eq!(back.current_epoch, album.current_epoch);
    assert_eq!(back.write_tier_seed64, album.write_tier_seed64);
    assert_eq!(back.admin_seed64, album.admin_seed64);
    let authority = back
        .authority
        .as_ref()
        .unwrap()
        .restore(writable, &back.held_epochs())
        .unwrap();
    assert!(authority.admin_chain_verifies());
    assert!(back.escrow_matches(&blob, &master));
}

/// A blob under another master key, another kind of master-sealed blob, and an escrow served
/// under the wrong album id are each refused with their own error.
#[test]
fn an_album_key_escrow_refuses_the_wrong_key_the_wrong_kind_and_the_wrong_album() {
    let master = MasterKey::generate();
    let (store, writable, recovered) = fixture();
    let blob = store.get(&writable).unwrap().seal_escrow(&master).unwrap();

    assert!(matches!(
        PersistedAlbum::open_escrow(&blob, &MasterKey::generate(), writable),
        Err(AlbumStoreError::Crypto(_))
    ));
    let keystore_shaped = master.seal(&cbor::to_canonical_vec(&store).unwrap());
    assert!(matches!(
        PersistedAlbum::open_escrow(&keystore_shaped, &master, writable),
        Err(AlbumStoreError::NotAnAlbumKeyEscrow)
    ));
    assert!(matches!(
        PersistedAlbum::open_escrow(&blob, &master, recovered),
        Err(AlbumStoreError::EscrowAlbumMismatch { expected, found })
            if expected == recovered && found == writable
    ));
}

/// An escrow matches the state it was sealed from and nothing else: a new epoch, another
/// album, or bytes that do not open all read as stale.
#[test]
fn an_album_key_escrow_matches_only_the_state_it_was_sealed_from() {
    let master = MasterKey::generate();
    let (mut store, writable, recovered) = fixture();
    let before = store.get(&recovered).unwrap().clone();
    let blob = before.seal_escrow(&master).unwrap();
    assert!(before.escrow_matches(&blob, &master));
    assert!(
        before.escrow_matches(&before.seal_escrow(&master).unwrap(), &master),
        "a second seal of one state differs in bytes and still matches"
    );

    store.merge_amks(recovered, "Restored", [(recovered, 3, [5; 32])], None);
    let after = store.get(&recovered).unwrap();
    assert!(
        !after.escrow_matches(&blob, &master),
        "a new epoch is stale"
    );
    assert!(!store.get(&writable).unwrap().escrow_matches(&blob, &master));
    assert!(!before.escrow_matches(b"not an escrow", &master));
    assert!(!before.escrow_matches(&blob, &MasterKey::generate()));
}

/// #837: a read-only album (restored from a backup: no signing seeds, perhaps no authority)
/// never supersedes an escrow that carries the seeds. On the same epochs the escrow covers
/// it ([`EscrowCoverage::Behind`]); with an extra local epoch neither may replace the other
/// ([`EscrowCoverage::Divergent`]).
#[test]
fn a_read_only_album_never_supersedes_an_escrow_that_carries_its_seeds() {
    let master = MasterKey::generate();
    let (store, writable, _) = fixture();
    let full = store.get(&writable).unwrap().clone();
    let escrow = full.seal_escrow(&master).unwrap();

    let bare = PersistedAlbum::recovered(writable, "Trip", full.amks.clone(), None);
    let with_authority =
        PersistedAlbum::recovered(writable, "Trip", full.amks.clone(), full.authority.clone());
    let mut no_admin = full.clone();
    no_admin.admin_seed64 = None;
    let mut no_write_tier = full.clone();
    no_write_tier.write_tier_seed64 = None;
    let mut no_authority = full.clone();
    no_authority.authority = None;
    for (label, local) in [
        ("no seeds, no authority", &bare),
        ("no seeds", &with_authority),
        ("no admin seed", &no_admin),
        ("no write-tier seed", &no_write_tier),
        ("no authority", &no_authority),
    ] {
        assert_eq!(
            local.escrow_coverage(&escrow, &master),
            EscrowCoverage::Behind,
            "{label}: same epochs, the escrow holds what this album lacks"
        );
    }

    let mut ahead = bare.clone();
    ahead.amks.push((writable, 2, [6; 32]));
    assert_eq!(
        ahead.escrow_coverage(&escrow, &master),
        EscrowCoverage::Divergent,
        "an extra epoch does not license dropping the escrowed seeds"
    );
}

/// #837 controls: replacing an escrow is still allowed whenever this album holds every
/// capability the escrow holds — a writable album adding an epoch or its seeds, and a
/// read-only album extending a read-only escrow.
#[test]
fn an_album_holding_every_escrowed_capability_still_supersedes_it() {
    let master = MasterKey::generate();
    let (store, writable, recovered) = fixture();
    let full = store.get(&writable).unwrap().clone();

    let bare = PersistedAlbum::recovered(writable, "Trip", full.amks.clone(), None);
    assert_eq!(
        full.escrow_coverage(&bare.seal_escrow(&master).unwrap(), &master),
        EscrowCoverage::Superseded,
        "a writable album adds its seeds and authority to a read-only escrow"
    );

    let mut full_ahead = full.clone();
    full_ahead.amks.push((writable, 2, [6; 32]));
    assert_eq!(
        full_ahead.escrow_coverage(&full.seal_escrow(&master).unwrap(), &master),
        EscrowCoverage::Superseded,
        "a writable album adds an epoch to its own escrow"
    );

    let read_only = store.get(&recovered).unwrap().clone();
    let mut read_only_ahead = read_only.clone();
    read_only_ahead.amks.push((recovered, 3, [5; 32]));
    assert_eq!(
        read_only_ahead.escrow_coverage(&read_only.seal_escrow(&master).unwrap(), &master),
        EscrowCoverage::Superseded,
        "a read-only album adds an epoch to a read-only escrow"
    );
}
