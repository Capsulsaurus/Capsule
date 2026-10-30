use super::*;

fn viewer() -> FfiRenderRequest {
    FfiRenderRequest {
        intent: FfiRepresentation::Preview,
        surface_width: 1000,
        surface_height: 800,
    }
}

/// The boundary carries the Rust answer through unchanged: an embedded preview that covers the
/// surface fills Preview, and a listed encoded Preview is fetched to replace it.
#[test]
fn render_step_round_trips_the_boundary() {
    let local = FfiLocalHoldings {
        held: vec![FfiRepresentation::Original],
        embedded_preview_width: Some(1000),
        embedded_preview_height: Some(667),
    };
    let step = render_step(viewer(), None, local, vec![FfiRepresentation::Preview]);
    assert_eq!(
        step,
        FfiRenderStep {
            show: FfiRendition {
                rung: FfiRepresentation::Preview,
                source: FfiRungSource::Embedded,
            },
            changed: true,
            next: Some(FfiNextStep::Fetch {
                representation: FfiRepresentation::Preview,
            }),
        }
    );
}

/// An embedded preview needs both dimensions; one alone is no preview.
#[test]
fn a_half_stated_embedded_preview_is_absent() {
    let local = FfiLocalHoldings {
        held: vec![FfiRepresentation::Original],
        embedded_preview_width: Some(4000),
        embedded_preview_height: None,
    };
    let step = render_step(viewer(), None, local, Vec::new());
    assert_eq!(step.show.source, FfiRungSource::Lqip);
    assert_eq!(
        step.next,
        Some(FfiNextStep::Generate {
            representation: FfiRepresentation::Preview,
        })
    );
}

/// What the caller shows is passed back, and the answer never drops below it.
#[test]
fn the_shown_rendition_is_a_floor() {
    let shown = FfiRendition {
        rung: FfiRepresentation::Thumbnail,
        source: FfiRungSource::Encoded,
    };
    let step = render_step(
        viewer(),
        Some(shown),
        FfiLocalHoldings::default(),
        Vec::new(),
    );
    assert_eq!(step.show, shown);
    assert!(!step.changed);
}

/// Every rung and source maps both ways.
#[test]
fn the_mirrors_are_bijections() {
    for rung in [
        FfiRepresentation::Lqip,
        FfiRepresentation::Thumbnail,
        FfiRepresentation::Preview,
        FfiRepresentation::Original,
    ] {
        assert_eq!(FfiRepresentation::from(Representation::from(rung)), rung);
    }
    for source in [
        FfiRungSource::Lqip,
        FfiRungSource::Embedded,
        FfiRungSource::Encoded,
        FfiRungSource::Original,
    ] {
        assert_eq!(FfiRungSource::from(RungSource::from(source)), source);
    }
}

/// A still that is not a RAW, and bytes that are no still, have no embedded preview; neither is
/// an error.
#[test]
fn a_non_raw_original_has_no_embedded_preview() {
    assert_eq!(
        embedded_preview_of(b"\xFF\xD8\xFF\xE0 not much", "jpg"),
        None
    );
    assert_eq!(embedded_preview_of(b"not an image", "txt"), None);
}

/// A file that cannot be read is the one error.
#[test]
fn an_unreadable_original_is_a_workspace_error() {
    let missing = std::env::temp_dir().join("capsule-ffi-ladder-missing.arw");
    let error = embedded_preview(missing.display().to_string(), "arw".to_owned())
        .expect_err("nothing to read");
    assert!(matches!(error, FfiError::Workspace { .. }), "{error}");
}

/// The extracted fields cross unchanged.
#[test]
fn an_embedded_preview_crosses_unchanged() {
    let preview = FfiEmbeddedPreview::from(EmbeddedPreview {
        jpeg: vec![0xFF, 0xD8],
        width: 24,
        height: 40,
        orientation: 6,
    });
    assert_eq!(preview.jpeg, [0xFF, 0xD8]);
    assert_eq!(
        (preview.width, preview.height, preview.orientation),
        (24, 40, 6)
    );
    assert!(
        !format!("{preview:?}").contains("216"),
        "the bytes are not logged"
    );
}
