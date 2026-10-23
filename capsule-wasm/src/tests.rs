use std::collections::HashSet;

use super::*;

/// The full variant set, written out so the exhaustive match below is meaningful.
fn every_sharing_error() -> [SharingError; 6] {
    [
        SharingError::ScopeUnavailable,
        SharingError::NotFound,
        SharingError::PassphraseRequired,
        SharingError::WrongPassphrase,
        SharingError::Crypto("kem"),
        SharingError::Malformed("record"),
    ]
}

#[test]
fn sharing_code_maps_each_variant() {
    assert_eq!(
        sharing_code(&SharingError::PassphraseRequired),
        err::PASSPHRASE_REQUIRED
    );
    assert_eq!(
        sharing_code(&SharingError::WrongPassphrase),
        err::WRONG_SECRET
    );
    assert_eq!(
        sharing_code(&SharingError::ScopeUnavailable),
        err::SCOPE_UNAVAILABLE
    );
    assert_eq!(sharing_code(&SharingError::NotFound), err::MALFORMED);
    assert_eq!(sharing_code(&SharingError::Crypto("kem")), err::MALFORMED);
    assert_eq!(
        sharing_code(&SharingError::Malformed("record")),
        err::MALFORMED
    );
}

/// The oracle property: on the open path a wrong passphrase and a wrong fragment secret
/// must be indistinguishable, so both collapse to `wrong_secret`. A viewer that could
/// tell them apart would report *which* half of the link was wrong.
#[test]
fn open_code_cannot_distinguish_a_wrong_passphrase_from_a_wrong_fragment() {
    assert_eq!(
        open_code(&SharingError::WrongPassphrase),
        open_code(&SharingError::Crypto("decapsulate"))
    );
    assert_eq!(open_code(&SharingError::WrongPassphrase), err::WRONG_SECRET);
    assert_eq!(
        open_code(&SharingError::PassphraseRequired),
        err::PASSPHRASE_REQUIRED
    );
    assert_eq!(open_code(&SharingError::ScopeUnavailable), err::MALFORMED);
    assert_eq!(open_code(&SharingError::NotFound), err::MALFORMED);
    // A record that opened but is not understood is not a wrong secret: telling the guest
    // their link is wrong would send them to re-check a link that is fine.
    assert_eq!(
        open_code(&SharingError::Malformed("record")),
        err::MALFORMED
    );
}

/// Every code the boundary can emit. The viewer maps exactly this set to catalog keys, so
/// a code outside it reaches the UI as an unmapped string.
const DECLARED_CODES: [&str; 6] = [
    err::MALFORMED,
    err::PASSPHRASE_REQUIRED,
    err::WRONG_SECRET,
    err::SCOPE_UNAVAILABLE,
    err::TAMPERED,
    err::SEAL_FAILED,
];

/// Two guards on adding a `SharingError` variant upstream. The match is exhaustive, so a
/// new variant fails the build here rather than reaching the viewer unmapped; and both
/// codes must be one of [`DECLARED_CODES`], so the arm you are forced to add cannot answer
/// with an ad-hoc literal the viewer has no catalog key for.
#[test]
fn every_sharing_error_variant_maps_to_a_declared_code() {
    let all = every_sharing_error();
    for e in &all {
        match e {
            SharingError::ScopeUnavailable
            | SharingError::NotFound
            | SharingError::PassphraseRequired
            | SharingError::WrongPassphrase
            | SharingError::Crypto(_)
            | SharingError::Malformed(_) => {}
        }
        assert!(
            DECLARED_CODES.contains(&sharing_code(e)),
            "sharing_code({e:?}) = {:?} is not a declared boundary code",
            sharing_code(e)
        );
        assert!(
            DECLARED_CODES.contains(&open_code(e)),
            "open_code({e:?}) = {:?} is not a declared boundary code",
            open_code(e)
        );
        assert!(
            DECLARED_CODES.contains(&recovery_code(e)),
            "recovery_code({e:?}) = {:?} is not a declared boundary code",
            recovery_code(e)
        );
    }

    // `every_sharing_error` is hand-written, so guard it against silently listing the same
    // variant twice and thereby covering one fewer than the array length claims. A set, not
    // `Vec::dedup`: that collapses only *consecutive* duplicates, so `[A, B, A]` would slip
    // through with its length unchanged.
    let kinds: HashSet<_> = all.iter().map(std::mem::discriminant).collect();
    assert_eq!(
        kinds.len(),
        all.len(),
        "every_sharing_error repeats a variant"
    );
}

/// #598: a wrapped key that does not open is an authentication failure, so the viewer
/// reports it as it reports a tampered blob; every other recovery error keeps its
/// `sharing_code`.
#[test]
fn recovery_code_reports_a_wrapped_key_that_does_not_open_as_tampered() {
    assert_eq!(
        recovery_code(&SharingError::Crypto("asset file key recovery failed")),
        err::TAMPERED
    );
    for e in every_sharing_error()
        .iter()
        .filter(|e| !matches!(e, SharingError::Crypto(_)))
    {
        assert_eq!(recovery_code(e), sharing_code(e), "{e:?}");
    }
}

#[test]
fn hex_array_decodes_a_canonical_32_byte_field() {
    let bytes: [u8; LINK_SECRET_LEN] = std::array::from_fn(|i| i as u8);
    let encoded = hex::encode(bytes);
    assert_eq!(encoded.len(), 64);

    // Surrounding whitespace is trimmed, as the browser hands the fragment over.
    let decoded = hex_array::<LINK_SECRET_LEN>(&format!("  {encoded}\n"))
        .expect("a canonical 64-char hex field decodes");
    assert_eq!(decoded, bytes);
}

// ── LQIP decode ───────────────────────────────────────────

/// A gradient frame, the shape `Lqip::encode` takes.
fn gradient(width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x + y) * 255 / (w + h)) as u8,
                255,
            ]);
        }
    }
    rgba
}

/// The browser decodes the same bytes the import pipeline encoded, to the same pixels the
/// core `decode_capped` produces — the cross-surface criterion, at the one boundary
/// where a second implementation could have crept in.
#[test]
fn decode_lqip_matches_the_core_decoder_for_a_real_payload() {
    use capsule_core::lqip::{Gamut, LQIP_FORMAT_V1, Lqip};

    let lqip = Lqip::encode(200, 150, &gradient(200, 150), Gamut::Srgb).expect("encode");
    let payload = lqip.as_bytes();
    assert_eq!(payload.len(), 32, "the committed tier is 32 bytes");

    let decoded = render_lqip_record(LQIP_FORMAT_V1, payload, &lqip.dominant_color(), 64, 64);
    assert_eq!(
        decoded,
        lqip.decode_capped(64, 64),
        "byte-identical to the core decoder"
    );
    assert_eq!(
        decoded.rgba.len() as u32,
        decoded.width * decoded.height * 4,
        "the buffer is exactly what `new ImageData(rgba, w, h)` requires"
    );
    assert!(
        decoded.width <= 64 && decoded.height <= 64,
        "the decode is capped to the box being painted, not to a fixed size"
    );
}

/// An unrecognised version and an undecodable payload both paint the stored fallback colour
/// rather than throwing: a reader must never misrender, and a missing placeholder is not
/// worth failing a gallery over.
#[test]
fn decode_lqip_falls_back_to_the_dominant_colour_instead_of_throwing() {
    use capsule_core::lqip::LQIP_FORMAT_V1;

    let fill = [12u8, 34, 56];
    for (version, payload) in [
        (LQIP_FORMAT_V1 + 999, vec![0xDE, 0xAD, 0xBE, 0xEF]),
        (LQIP_FORMAT_V1, vec![0xDE, 0xAD, 0xBE, 0xEF]),
        (LQIP_FORMAT_V1, Vec::new()),
    ] {
        let decoded = render_lqip_record(version, &payload, &fill, 32, 32);
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.rgba, vec![12, 34, 56, 255]);
    }
}

/// A malformed `dominant_color` paints [`FALLBACK_FILL`] rather than throwing — the same
/// answer `capsule-core-ffi`'s `render_lqip` gives, so a viewer's behaviour does not depend
/// on which client opened the record.
#[test]
fn decode_lqip_paints_the_fallback_fill_for_a_malformed_dominant_colour() {
    use capsule_core::lqip::LQIP_FORMAT_V1;

    for fill in [&[][..], &[1][..], &[1, 2][..], &[1, 2, 3, 4][..]] {
        let decoded = render_lqip_record(LQIP_FORMAT_V1, &[0; 32], fill, 16, 16);
        assert_eq!(
            (decoded.width, decoded.height),
            (1, 1),
            "a {}-byte dominant_color paints the fill, not an error",
            fill.len()
        );
        assert_eq!(
            decoded.rgba,
            vec![FALLBACK_FILL[0], FALLBACK_FILL[1], FALLBACK_FILL[2], 255],
            "and it is black — identical to the native surface"
        );
    }
}

/// #614: the metadata view hands the viewer exactly the shapes `decryptBlob` and the serve
/// path take, and its link scope is the base64 `openShare` opens.
#[test]
fn the_metadata_view_projects_the_record_in_the_shapes_the_viewer_takes() {
    use capsule_core::crypto::hash::Hash32;
    use capsule_core::sharing::{encapsulate_scope, publish_share};

    let fragment = [0xA1u8; LINK_SECRET_LEN];
    let opaque_id = [0x5Eu8; OPAQUE_ID_LEN];
    let file_id = Uuid::from_u128(7);
    let material = ScopeMaterial::Asset {
        file_id,
        file_key: [0x42; 32],
    };
    let fast = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, None, fast).unwrap();
    let wrapped_key = WrappedFileKey(vec![0xCD; WRAPPED_FILE_KEY_LEN]);
    let asset = SharedAsset {
        asset_id: file_id,
        content_hash: Hash32([0xEE; 32]),
        content_type: "image/png".to_owned(),
        size: 5008,
        amk_version: 3,
        nonce_prefix: [0x07; NONCE_PREFIX_LEN],
        key_mode: KeyMode::Wrapped,
        wrapped_file_key: Some(wrapped_key.clone()),
    };
    let publication = publish_share(
        &wrapped,
        &fragment,
        &opaque_id,
        Some("2030-01-01T00:00:00Z".to_owned()),
        vec![asset],
    )
    .unwrap();

    let view = open_share_metadata_wasm(
        &publication.metadata_blob,
        &hex::encode(opaque_id),
        &hex::encode(fragment),
    )
    .expect("the record opens under its own URL");
    assert_eq!(view.expires_at().as_deref(), Some("2030-01-01T00:00:00Z"));

    let scope = view.link_scope_b64().unwrap().expect("a link-only scope");
    let opened = open_share(
        &scope,
        &hex::encode(opaque_id),
        &hex::encode(fragment),
        None,
    )
    .expect("openShare takes the projected scope");
    assert_eq!(opened.scope_kind(), "asset");

    let assets = view.assets();
    assert_eq!(assets.len(), 1);
    let a = &assets[0];
    assert_eq!(a.asset_id(), file_id.to_string());
    assert_eq!(a.content_hash(), "ee".repeat(32));
    assert_eq!(a.content_type(), "image/png");
    assert_eq!(a.size().to_bits(), 5008f64.to_bits());
    assert_eq!(a.amk_version(), 3);
    assert_eq!(a.nonce_prefix_hex(), "07".repeat(NONCE_PREFIX_LEN));
    assert_eq!(a.key_mode(), "wrapped");
    assert_eq!(a.wrapped_file_key_hex(), Some(hex::encode(&wrapped_key.0)));
}

#[test]
fn decode_wrapped_round_trips_a_canonical_wrapped_scope() {
    let wrapped = WrappedScope::LinkOnly {
        blob: b"sealed-scope-material".to_vec(),
    };
    let cbor = capsule_core::cbor::to_canonical_vec(&wrapped).expect("canonical CBOR");
    let b64 = BASE64.encode(&cbor);

    let decoded = decode_wrapped(&b64).expect("the material the serve path returns decodes");
    assert_eq!(decoded, wrapped);
    assert!(!decoded.is_passphrase_protected());
}
