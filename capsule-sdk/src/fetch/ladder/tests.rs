use Representation::{Lqip, Original, Preview, Thumbnail};

use super::*;

const PHONE: PixelSize = PixelSize {
    width: 1170,
    height: 2532,
};

fn request(intent: Representation) -> RenderRequest {
    RenderRequest {
        intent,
        surface: PHONE,
    }
}

fn holding(held: &[Representation], embedded: Option<(u32, u32)>) -> LocalHoldings {
    LocalHoldings {
        held: held.iter().copied().collect(),
        embedded_preview: embedded.map(|(width, height)| PixelSize { width, height }),
    }
}

fn set(reps: &[Representation]) -> BTreeSet<Representation> {
    reps.iter().copied().collect()
}

const fn rendition(rung: Representation, source: RungSource) -> Rendition {
    Rendition { rung, source }
}

#[test]
fn renditions_order_by_rung_then_encoded_above_embedded() {
    let ladder = [
        Rendition::LQIP,
        rendition(Thumbnail, RungSource::Embedded),
        rendition(Thumbnail, RungSource::Encoded),
        rendition(Preview, RungSource::Embedded),
        rendition(Preview, RungSource::Encoded),
        rendition(Original, RungSource::Original),
    ];
    assert!(ladder.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn coverage_is_an_aspect_fit_without_upscaling() {
    let surface = PixelSize {
        width: 1000,
        height: 800,
    };
    let landscape = PixelSize {
        width: 1000,
        height: 500,
    };
    let portrait = PixelSize {
        width: 400,
        height: 800,
    };
    let small = PixelSize {
        width: 999,
        height: 799,
    };
    assert!(landscape.covers(surface));
    assert!(portrait.covers(surface));
    assert!(!small.covers(surface));
    assert!(small.covers(PixelSize::default()));
    assert_eq!(portrait.long_edge(), 800);
}

#[test]
fn with_nothing_held_the_lqip_shows_and_the_intent_is_fetched() {
    let step = render_step(
        &request(Thumbnail),
        None,
        &holding(&[], None),
        &set(&[Thumbnail]),
    );
    assert_eq!(step.show, Rendition::LQIP);
    assert!(step.changed);
    assert_eq!(step.next, Some(NextStep::Fetch(Thumbnail)));
}

#[test]
fn the_best_rung_at_or_below_the_intent_is_shown() {
    let local = holding(&[Thumbnail, Preview, Original], None);
    let grid = render_step(&request(Thumbnail), None, &local, &set(&[]));
    assert_eq!(grid.show, rendition(Thumbnail, RungSource::Encoded));
    assert_eq!(grid.next, None, "the grid never climbs past its intent");

    let viewer = render_step(&request(Preview), None, &local, &set(&[]));
    assert_eq!(viewer.show, rendition(Preview, RungSource::Encoded));
    assert_eq!(viewer.next, None);

    let full = render_step(&request(Original), None, &local, &set(&[]));
    assert_eq!(full.show, rendition(Original, RungSource::Original));
    assert_eq!(full.next, None);
}

#[test]
fn an_embedded_preview_fills_thumbnail_from_the_tier_long_edge() {
    let edge = thumbnail_long_edge();
    assert_eq!(edge, 256);
    let reaching = holding(&[], Some((edge, edge / 2)));
    let step = render_step(&request(Thumbnail), None, &reaching, &set(&[]));
    assert_eq!(step.show, rendition(Thumbnail, RungSource::Embedded));
    assert_eq!(
        step.next, None,
        "nothing to fetch and generation is the import's"
    );

    let short = holding(&[], Some((edge - 1, edge / 2)));
    let step = render_step(&request(Thumbnail), None, &short, &set(&[]));
    assert_eq!(step.show, Rendition::LQIP);
}

#[test]
fn an_encoded_tier_replaces_the_embedded_preview_on_its_rung() {
    let local = holding(&[Thumbnail], Some((4000, 3000)));
    let step = render_step(&request(Thumbnail), None, &local, &set(&[]));
    assert_eq!(step.show, rendition(Thumbnail, RungSource::Encoded));

    let local = holding(&[Preview, Original], Some((4000, 3000)));
    let step = render_step(&request(Preview), None, &local, &set(&[]));
    assert_eq!(step.show, rendition(Preview, RungSource::Encoded));
}

#[test]
fn an_embedded_preview_fills_preview_only_when_it_covers_the_surface() {
    let covering = holding(&[Original], Some((PHONE.height, PHONE.width * 2)));
    let step = render_step(&request(Preview), None, &covering, &set(&[]));
    assert_eq!(step.show, rendition(Preview, RungSource::Embedded));
    assert_eq!(
        step.next, None,
        "generating the encoded Preview is the import's work"
    );

    // Too small for the surface but large enough for the grid: it drops to Thumbnail, and the
    // held original generates the Preview.
    let small = holding(&[Original], Some((1024, 683)));
    let step = render_step(&request(Preview), None, &small, &set(&[]));
    assert_eq!(step.show, rendition(Thumbnail, RungSource::Embedded));
    assert_eq!(step.next, Some(NextStep::Generate(Preview)));
}

#[test]
fn an_embedded_preview_is_replaced_by_a_fetchable_encoded_tier() {
    let covering = holding(&[Original], Some((4000, 3000)));
    let step = render_step(&request(Preview), None, &covering, &set(&[Preview]));
    assert_eq!(step.show, rendition(Preview, RungSource::Embedded));
    assert_eq!(step.next, Some(NextStep::Fetch(Preview)));
}

#[test]
fn an_embedded_rung_above_the_intent_asks_for_nothing() {
    // A surface asking for Thumbnail passes back the embedded Preview a larger view showed.
    let shown = rendition(Preview, RungSource::Embedded);
    let local = holding(&[Original], Some((4000, 3000)));
    let fetchable = set(&[Thumbnail, Preview]);
    let step = render_step(&request(Thumbnail), Some(shown), &local, &fetchable);
    assert_eq!(step.show, shown);
    assert_eq!(step.next, None);

    // Holding the Thumbnail tier changes nothing: it ranks below what is shown.
    let held = holding(&[Thumbnail, Original], Some((4000, 3000)));
    let step = render_step(&request(Thumbnail), Some(shown), &held, &fetchable);
    assert_eq!(step.show, shown);
    assert!(!step.changed);
    assert_eq!(step.next, None);

    // An embedded rung exactly at the intent still asks for its encoded tier.
    let step = render_step(&request(Thumbnail), None, &local, &fetchable);
    assert_eq!(step.show, rendition(Thumbnail, RungSource::Embedded));
    assert_eq!(step.next, Some(NextStep::Fetch(Thumbnail)));
}

#[test]
fn a_fetch_is_preferred_over_generating_from_the_original() {
    let local = holding(&[Original], None);
    let step = render_step(&request(Preview), None, &local, &set(&[Preview]));
    assert_eq!(step.next, Some(NextStep::Fetch(Preview)));
    let step = render_step(&request(Preview), None, &local, &set(&[]));
    assert_eq!(step.next, Some(NextStep::Generate(Preview)));
}

#[test]
fn the_original_is_only_fetched_never_generated() {
    let step = render_step(
        &request(Original),
        None,
        &holding(&[Preview], None),
        &set(&[]),
    );
    assert_eq!(step.show, rendition(Preview, RungSource::Encoded));
    assert_eq!(step.next, None);
    let step = render_step(
        &request(Original),
        None,
        &holding(&[Preview], None),
        &set(&[Original]),
    );
    assert_eq!(step.next, Some(NextStep::Fetch(Original)));
}

#[test]
fn an_lqip_intent_needs_nothing() {
    let step = render_step(
        &request(Lqip),
        None,
        &holding(&[Original], None),
        &set(&[Thumbnail]),
    );
    assert_eq!(step.show, Rendition::LQIP);
    assert_eq!(step.next, None);
}

#[test]
fn a_view_upgrades_in_place_and_never_downgrades() {
    let req = request(Preview);
    let first = render_step(&req, None, &holding(&[], None), &set(&[Thumbnail, Preview]));
    assert_eq!(first.show, Rendition::LQIP);

    let thumb = render_step(
        &req,
        Some(first.show),
        &holding(&[Thumbnail], None),
        &set(&[Preview]),
    );
    assert_eq!(thumb.show, rendition(Thumbnail, RungSource::Encoded));
    assert!(thumb.changed);
    assert_eq!(thumb.next, Some(NextStep::Fetch(Preview)));

    // The thumbnail is evicted while the view is open: what is on screen stays.
    let evicted = render_step(
        &req,
        Some(thumb.show),
        &holding(&[], None),
        &set(&[Preview]),
    );
    assert_eq!(evicted.show, thumb.show);
    assert!(!evicted.changed);

    let preview = render_step(
        &req,
        Some(evicted.show),
        &holding(&[Preview], None),
        &set(&[]),
    );
    assert_eq!(preview.show, rendition(Preview, RungSource::Encoded));
    assert!(preview.changed);
    assert_eq!(preview.next, None);
}

#[test]
fn falling_back_is_a_step_with_nothing_shown() {
    let req = request(Preview);
    let shown = rendition(Preview, RungSource::Encoded);
    // The held preview would not open: the surface falls back to what remains.
    let step = render_step(&req, None, &holding(&[Thumbnail], None), &set(&[]));
    assert!(step.show < shown);
    assert_eq!(step.show, rendition(Thumbnail, RungSource::Encoded));
}

#[test]
fn a_view_that_grows_keeps_the_embedded_preview_it_opened_with() {
    // The view opened on a surface the embedded preview covers; a later, larger window keeps
    // passing the size it opened at, so the rung holds.
    let opened = RenderRequest {
        intent: Preview,
        surface: PixelSize {
            width: 1600,
            height: 1000,
        },
    };
    let local = holding(&[Original], Some((1600, 1067)));
    let step = render_step(&opened, None, &local, &set(&[]));
    assert_eq!(step.show, rendition(Preview, RungSource::Embedded));

    // Had it opened at the larger size, the embedded preview would only reach Thumbnail.
    let larger = RenderRequest {
        intent: Preview,
        surface: PixelSize {
            width: 3200,
            height: 2000,
        },
    };
    let step = render_step(&larger, None, &local, &set(&[]));
    assert_eq!(step.show, rendition(Thumbnail, RungSource::Embedded));
    // ...yet once shown, the larger request never takes it away.
    let kept = render_step(
        &larger,
        Some(rendition(Preview, RungSource::Embedded)),
        &local,
        &set(&[]),
    );
    assert_eq!(kept.show, rendition(Preview, RungSource::Embedded));
    assert!(!kept.changed);
}
