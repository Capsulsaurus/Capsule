use super::*;

/// **The reason this module is not in `media`.** These tests compile and run under
/// `--no-default-features`, which is how `capsule-server` and `capsule-wasm` build — the two
/// crates that receive a `DerivativeManifest` they did not author. A closed set only its
/// producer can evaluate is not a closed set.
///
/// This test asserts nothing a caller could not, and that is the point: it exists so the
/// *linkage* is exercised by the `--no-default-features` build rather than assumed.
#[test]
fn the_closed_set_is_evaluable_without_the_media_feature() {
    for format in [
        DerivativeFormat::Jxl,
        DerivativeFormat::Avif,
        DerivativeFormat::WebP,
        DerivativeFormat::Original,
    ] {
        assert_eq!(DerivativeFormat::parse(format.mime()), Some(format));
    }
    assert!(!DerivativeFormat::is_recognized("image/future-codec"));
    assert!(!DerivativeFormat::is_recognized("embedding/mobileclip-b"));
}

/// The key-free pair check is the closed set itself: every still role accepts exactly the
/// recognised formats, refuses everything else by naming it, and an embedding role is out of
/// the set's scope rather than a violation of it.
#[test]
fn the_key_free_pair_check_accepts_the_closed_set_and_names_the_rest() {
    for role in [DerivativeRole::Thumbnail, DerivativeRole::Preview] {
        for format in [
            DerivativeFormat::Jxl,
            DerivativeFormat::Avif,
            DerivativeFormat::WebP,
            DerivativeFormat::Original,
        ] {
            assert_eq!(
                verify_still_role_format(role, format.mime()),
                Ok(Some(format))
            );
        }
        for unknown in ["image/future-codec", "image/jpeg", "IMAGE/JXL", ""] {
            assert_eq!(
                verify_still_role_format(role, unknown),
                Err(unknown.to_owned()),
                "{role:?} must refuse {unknown:?}"
            );
        }
    }
    assert_eq!(
        verify_still_role_format(DerivativeRole::Embedding, "embedding/mobileclip-b"),
        Ok(None)
    );
    assert_eq!(
        verify_still_role_format(DerivativeRole::Embedding, "image/future-codec"),
        Ok(None),
        "the embedding grammar is not this set's to judge"
    );
}
