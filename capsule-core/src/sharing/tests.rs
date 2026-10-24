use super::*;
use crate::crypto::primitives::Argon2Params;

/// Tiny Argon2id parameters keep the passphrase-wrap tests fast.
fn fast() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

fn sample_material() -> ScopeMaterial {
    ScopeMaterial::Asset {
        file_id: Uuid::now_v7(),
        file_key: [0x42; 32],
    }
}

/// The acceptance / doc "Opaque-id entropy (unit)": generated opaque ids are
/// 128-bit CSPRNG values, never structured; a generator producing shorter or
/// guessable/sequential ids fails.
#[test]
fn opaque_id_entropy() {
    // A full 128 bits — not a shorter identifier.
    assert_eq!(OPAQUE_ID_LEN, 16, "opaque id must be a full 128 bits");
    assert_eq!(generate_opaque_id().len(), 16);

    // Draw a batch: all distinct (no collisions), and — unlike a UUIDv7 — carrying no
    // monotonic timestamp prefix. A structured/sequential generator would fail both.
    let ids: Vec<[u8; OPAQUE_ID_LEN]> = (0..256).map(|_| generate_opaque_id()).collect();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(ids[i], ids[j], "two CSPRNG opaque ids must never collide");
        }
    }
    // No UUIDv7 version/variant structure: the version nibble (byte 6 high nibble) and
    // variant bits (byte 8) are not pinned to 0x7_ / 0b10 as a v7 generator forces.
    assert!(
        ids.iter().any(|id| (id[6] >> 4) != 0x7),
        "opaque ids must not all carry the UUIDv7 version nibble"
    );
    assert!(
        ids.iter().any(|id| (id[8] & 0xc0) != 0x80),
        "opaque ids must not all carry the UUID variant bits"
    );
    // Not sequential: the leading 48 bits (a v7 timestamp field) are not monotonic.
    let high48 =
        |id: &[u8; OPAQUE_ID_LEN]| id[..6].iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
    assert!(
        ids.windows(2).any(|w| high48(&w[0]) >= high48(&w[1])),
        "opaque ids must not be time-ordered/sequential like a UUIDv7"
    );
}

/// The acceptance / doc "Passphrase unwrap locality (unit)": with a passphrase set,
/// the server-held material is the **wrapped** form and unwraps **client-side** via the
/// password-based KDF; the passphrase itself never appears in any wire-bound structure.
#[test]
fn passphrase_unwrap_is_client_side() {
    const PASSPHRASE: &str = "correct horse battery staple";
    let material = sample_material();
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();

    let wrapped =
        encapsulate_scope(&material, &fragment, &opaque_id, Some(PASSPHRASE), fast()).unwrap();

    // The server holds only the wrapped form — an Argon2id passphrase layer.
    let WrappedScope::Passphrase { wrapped: inner } = &wrapped else {
        panic!("a passphrase-protected link must yield the wrapped form");
    };
    // Argon2id really ran (self-describing cost params recorded in-band).
    assert_eq!(inner.mem_kib, fast().mem_kib);
    assert_eq!(inner.t_cost, fast().t_cost);

    // The passphrase never appears in any wire-bound byte of the server-held material.
    let pw = PASSPHRASE.as_bytes();
    let contains = |h: &[u8]| h.windows(pw.len()).any(|w| w == pw);
    assert!(
        !contains(&inner.ciphertext),
        "passphrase must not be in ciphertext"
    );
    assert!(!contains(&inner.salt), "passphrase must not be in the salt");
    assert!(
        !contains(&inner.nonce),
        "passphrase must not be in the nonce"
    );

    // Client-side unwrap with the passphrase + fragment recovers the exact material.
    let opened = open_scope(&wrapped, &opaque_id, &fragment, Some(PASSPHRASE)).unwrap();
    assert_eq!(opened, material);

    // A wrong passphrase is rejected client-side (the Argon2id backstop) ...
    assert_eq!(
        open_scope(&wrapped, &opaque_id, &fragment, Some("wrong")),
        Err(SharingError::WrongPassphrase),
    );
    // ... and opening a passphrase link with no passphrase is refused, not silently served.
    assert_eq!(
        open_scope(&wrapped, &opaque_id, &fragment, None),
        Err(SharingError::PassphraseRequired),
    );
}

/// The unprotected path: the scope material round-trips under the fragment secret
/// alone, and the encapsulation is bound to its opaque id.
#[test]
fn link_only_round_trip_is_bound_to_opaque_id() {
    let material = ScopeMaterial::Album {
        album_id: Uuid::now_v7(),
        amks: BTreeMap::from([(1u32, [1u8; 32]), (2u32, [2u8; 32])]),
    };
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();
    let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, None, fast()).unwrap();
    assert!(!wrapped.is_passphrase_protected());

    // Correct fragment + opaque id → recovers the material.
    assert_eq!(
        open_scope(&wrapped, &opaque_id, &fragment, None).unwrap(),
        material,
    );
    // A different opaque id (the HKDF salt) does not open it.
    assert!(open_scope(&wrapped, &generate_opaque_id(), &fragment, None).is_err());
    // A different fragment secret does not open it.
    let other = rng::random_array::<LINK_SECRET_LEN>();
    assert!(open_scope(&wrapped, &opaque_id, &other, None).is_err());
}

fn shared_asset(tag: u8) -> SharedAsset {
    SharedAsset {
        asset_id: Uuid::from_u128(u128::from(tag)),
        content_hash: crate::crypto::hash::Hash32([tag; 32]),
        content_type: "image/png".to_owned(),
        size: 5008,
        amk_version: 0,
        nonce_prefix: [tag; 7],
        key_mode: crate::crypto::provenance::KeyMode::Derived,
        wrapped_file_key: None,
        sidecar: None,
    }
}

/// A sidecar for the asset `shared_asset(tag)` names.
#[cfg(feature = "native")]
fn sidecar_for(tag: u8) -> crate::sidecar::SidecarV1 {
    use crate::metadata::crdt::{Lww, OrSet};
    crate::sidecar::SidecarV1 {
        sidecar_schema: crate::sidecar::SIDECAR_SCHEMA_V1,
        crypto_suite_id: crate::crypto::primitives::CRYPTO_SUITE_ID,
        uuid: Uuid::from_u128(u128::from(tag)),
        hash: crate::crypto::hash::Hash32([tag; 32]),
        capture_timestamp: "2026-05-31T10:00:00Z".into(),
        capture_timestamp_user: Lww::new(),
        import_timestamp: "2026-05-31T11:00:00Z".into(),
        content_type: "image/png".into(),
        dimensions: None,
        lqip: None,
        tags_user: OrSet::new(),
        tags_ai: OrSet::new(),
        caption: Lww::new(),
        rating: Lww::new(),
        stack_membership: Lww::new(),
        regions: crate::sidecar::Regions::new(),
        cull: Lww::new(),
        hidden: Lww::new(),
        camera_id: None,
        device_id: Uuid::nil(),
        session_id: Uuid::nil(),
        gps: None,
        gps_user: Lww::new(),
        provenance_chain_hash: None,
        unknown: BTreeMap::new(),
        signature: None,
    }
}

/// #432: an asset's export sidecar travels sealed inside the record and decodes back on the
/// recipient side; a record without one is unchanged on the wire.
#[test]
#[cfg(feature = "native")]
fn an_export_sidecar_travels_inside_the_sealed_record() {
    let material = sample_material();
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();
    let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, None, fast()).unwrap();
    let sidecar = sidecar_for(1);
    let with = SharedAsset {
        sidecar: Some(sidecar.to_canonical_vec()),
        ..shared_asset(1)
    };

    let publication = publish_share(&wrapped, &fragment, &opaque_id, None, vec![with]).unwrap();
    let record = open_share_metadata(&publication.metadata_blob, &opaque_id, &fragment).unwrap();
    assert_eq!(record.assets[0].exported_sidecar(), Ok(Some(sidecar)));

    let without = cbor::to_canonical_vec(&shared_asset(1)).unwrap();
    assert!(
        !without.windows(7).any(|w| w == b"sidecar"),
        "an asset with no sidecar has no sidecar key"
    );
    assert_eq!(shared_asset(1).exported_sidecar(), Ok(None));
}

/// #432: a sidecar that does not decode, or names another asset, is refused by the reader.
#[test]
#[cfg(feature = "native")]
fn a_foreign_or_corrupt_export_sidecar_is_malformed() {
    let foreign = SharedAsset {
        sidecar: Some(sidecar_for(2).to_canonical_vec()),
        ..shared_asset(1)
    };
    assert!(matches!(
        foreign.exported_sidecar(),
        Err(SharingError::Malformed(_))
    ));
    let corrupt = SharedAsset {
        sidecar: Some(vec![0xFF, 0x00]),
        ..shared_asset(1)
    };
    assert!(matches!(
        corrupt.exported_sidecar(),
        Err(SharingError::Malformed(_))
    ));
}

/// #614: a link-only link carries its scope inside the sealed record, which opens with the
/// URL alone; nothing goes to the server's wrapped-secret slot.
#[test]
fn a_link_only_publication_seals_its_scope_into_the_record() {
    let material = sample_material();
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();
    let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, None, fast()).unwrap();
    let assets = vec![shared_asset(1), shared_asset(2)];

    let publication = publish_share(
        &wrapped,
        &fragment,
        &opaque_id,
        Some("2030-01-01T00:00:00Z".to_owned()),
        assets.clone(),
    )
    .unwrap();
    assert_eq!(publication.wrapped_secret, None);
    assert_eq!(
        publication.metadata_hash,
        crate::crypto::hash::hash_bytes(&publication.metadata_blob),
        "the metadata hash is the blob's content address"
    );
    assert_eq!(
        publication.serves,
        vec![
            publication.metadata_hash,
            assets[0].content_hash,
            assets[1].content_hash
        ],
    );

    let record = open_share_metadata(&publication.metadata_blob, &opaque_id, &fragment).unwrap();
    assert_eq!(record.version, SHARE_METADATA_VERSION);
    assert_eq!(record.assets, assets);
    assert_eq!(record.expires_at.as_deref(), Some("2030-01-01T00:00:00Z"));
    let scope = record
        .link_scope
        .expect("a link-only record carries its scope");
    assert_eq!(
        open_scope(&scope, &opaque_id, &fragment, None).unwrap(),
        material
    );
}

/// #614: a passphrase link's scope never enters the fragment-sealed record — it goes to the
/// server's wrapped-secret slot, as the CBOR the browser decodes.
#[test]
fn a_passphrase_publication_keeps_its_scope_out_of_the_record() {
    let material = sample_material();
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();
    let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, Some("pw"), fast()).unwrap();

    let publication =
        publish_share(&wrapped, &fragment, &opaque_id, None, vec![shared_asset(3)]).unwrap();
    let record = open_share_metadata(&publication.metadata_blob, &opaque_id, &fragment).unwrap();
    assert_eq!(
        record.link_scope, None,
        "the fragment alone must not open it"
    );

    let secret = publication
        .wrapped_secret
        .expect("a passphrase link has a wrapped secret");
    let served: WrappedScope = cbor::from_slice(&secret).unwrap();
    assert_eq!(served, wrapped);
    assert_eq!(
        open_scope(&served, &opaque_id, &fragment, Some("pw")).unwrap(),
        material
    );
}

/// The record is bound to its URL: another fragment, another opaque id, or a flipped byte
/// does not open it, and all three are the same `Crypto` refusal.
#[test]
fn share_metadata_opens_only_under_its_own_url() {
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();
    let wrapped =
        encapsulate_scope(&sample_material(), &fragment, &opaque_id, None, fast()).unwrap();
    let blob = publish_share(&wrapped, &fragment, &opaque_id, None, vec![])
        .unwrap()
        .metadata_blob;

    let other_fragment = rng::random_array::<LINK_SECRET_LEN>();
    let mut tampered = blob.clone();
    *tampered.last_mut().unwrap() ^= 1;
    for (blob, id, secret) in [
        (&blob, &generate_opaque_id(), &fragment),
        (&blob, &opaque_id, &other_fragment),
        (&tampered, &opaque_id, &fragment),
    ] {
        assert!(matches!(
            open_share_metadata(blob, id, secret),
            Err(SharingError::Crypto(_))
        ));
    }
}

/// A record that opens but breaks a rule is `Malformed`, never silently accepted: an unknown
/// version, a passphrase scope inside the record, a key mode at odds with its wrapped key.
#[test]
fn a_record_that_breaks_a_rule_is_malformed() {
    let fragment = rng::random_array::<LINK_SECRET_LEN>();
    let opaque_id = generate_opaque_id();
    let seal = |record: &ShareMetadata| {
        seal_blob(
            &metadata_key(&fragment, &opaque_id),
            &cbor::to_canonical_vec(record).unwrap(),
        )
    };
    let good = ShareMetadata {
        version: SHARE_METADATA_VERSION.to_owned(),
        expires_at: None,
        link_scope: None,
        assets: vec![shared_asset(4)],
    };
    assert!(open_share_metadata(&seal(&good), &opaque_id, &fragment).is_ok());

    let mut unknown = good.clone();
    unknown.version = "share-metadata/v2".to_owned();
    let mut passphrase = good.clone();
    passphrase.link_scope = Some(
        encapsulate_scope(
            &sample_material(),
            &fragment,
            &opaque_id,
            Some("pw"),
            fast(),
        )
        .unwrap(),
    );
    let mut mode = good.clone();
    mode.assets[0].key_mode = crate::crypto::provenance::KeyMode::Wrapped;
    for record in [unknown, passphrase, mode.clone()] {
        assert!(matches!(
            open_share_metadata(&seal(&record), &opaque_id, &fragment),
            Err(SharingError::Malformed(_))
        ));
    }
    let not_a_record = seal_blob(&metadata_key(&fragment, &opaque_id), b"\x01");
    assert!(matches!(
        open_share_metadata(&not_a_record, &opaque_id, &fragment),
        Err(SharingError::Malformed(_))
    ));

    // The issuer refuses to seal the key-mode case in the first place.
    let wrapped =
        encapsulate_scope(&sample_material(), &fragment, &opaque_id, None, fast()).unwrap();
    assert!(matches!(
        publish_share(&wrapped, &fragment, &opaque_id, None, mode.assets),
        Err(SharingError::Malformed(_))
    ));
}

/// The metadata key and the scope-wrap key are separate HKDF labels over the same inputs.
#[test]
fn the_metadata_key_is_not_the_scope_wrap_key() {
    let fragment = [7u8; LINK_SECRET_LEN];
    let opaque_id = [9u8; OPAQUE_ID_LEN];
    assert_ne!(
        metadata_key(&fragment, &opaque_id),
        link_wrap_key(&fragment, &opaque_id)
    );
}

/// Album material derives per-epoch file keys; asset material returns its one key.
#[test]
fn scope_material_derives_file_keys() {
    let album_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let amks = BTreeMap::from([(1u32, [7u8; 32]), (2u32, [9u8; 32])]);
    let album = ScopeMaterial::Album {
        album_id,
        amks: amks.clone(),
    };
    let np = [0xEEu8; 7];
    // Album epoch key matches a direct AMK derivation under the same folded nonce prefix ...
    assert_eq!(
        album.file_key_for(&file_id, 1, &np).unwrap(),
        Amk::from_bytes(amks[&1]).derive_file_key(&file_id, &np),
    );
    // ... a distinct nonce prefix re-rolls it (the fold) ...
    assert_ne!(
        album.file_key_for(&file_id, 1, &np).unwrap(),
        album.file_key_for(&file_id, 1, &[0x11u8; 7]).unwrap(),
    );
    // ... and an epoch the material does not cover is unavailable.
    assert_eq!(
        album.file_key_for(&file_id, 3, &np),
        Err(SharingError::ScopeUnavailable),
    );

    let asset = ScopeMaterial::Asset {
        file_id,
        file_key: [3u8; 32],
    };
    // An asset grant carries the folded key; the nonce-prefix argument is unused.
    assert_eq!(asset.file_key_for(&file_id, 1, &np).unwrap(), [3u8; 32]);
    // A different asset id is not covered by a single-asset grant.
    assert_eq!(
        asset.file_key_for(&Uuid::now_v7(), 1, &np),
        Err(SharingError::ScopeUnavailable),
    );
}
