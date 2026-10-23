//! Metadata repair tests: the context around an asset, the time deltas, every correction it
//! asks the port to write, and the frame.

use std::path::PathBuf;

use capsule_core::sidecar::GpsSource;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::label_editor::Edited;
use crate::tui::library::LibraryAction;
use crate::tui::state::{Action, AppState, Screen};
use crate::tui::view;

const ALBUM: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00a1);
const LIBRARY: &str = "/photos/lib";

const fn id(n: u128) -> Uuid {
    Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_0100 + n)
}

const PARIS: Place = Place {
    lat: 48.8584,
    lon: 2.2945,
    datum: GpsDatum::Wgs84,
    source: GpsSource::Exif,
};

fn entry(n: u128, captured: &str) -> AssetEntry {
    AssetEntry {
        asset_id: id(n),
        album_id: ALBUM,
        captured: captured.parse().ok(),
        ext: "jpg".to_owned(),
        original: None,
        hidden: false,
        in_trash: false,
        stack: None,
        rows: Vec::new(),
        history: Vec::new(),
        labels: Vec::new(),
        regions: Vec::new(),
        capture_text: captured.to_owned(),
        capture_imported: None,
        location: None,
        location_imported: None,
        location_edited: false,
    }
}

/// Six assets an hour apart, newest first; the second (id 2) has a location, the fourth
/// (id 4) is in trash.
fn catalogue() -> Catalogue {
    let mut assets: Vec<AssetEntry> = (1..=6)
        .map(|n| entry(n, &format!("2024-06-01T{:02}:00:00Z", 20 - n)))
        .collect();
    assets[1].location = Some(PARIS);
    assets[3].in_trash = true;
    Catalogue::new(assets, Vec::new(), Vec::new())
}

fn type_text(state: &mut AppState, text: &str) {
    for c in text.chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
}

fn clear_field(state: &mut AppState) {
    for _ in 0..64 {
        state.update(Action::Form(FormKey::Backspace));
    }
}

/// The browser open on `catalogue`, with the timeline's row `row` selected.
fn browsing(row: usize) -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Library));
    state.update(Action::Library(LibraryAction::Edit));
    type_text(&mut state, LIBRARY);
    state.update(Action::Form(FormKey::Next));
    type_text(&mut state, "pw");
    state.update(Action::Form(FormKey::Submit));
    state.update(Action::Library(LibraryAction::Opened(Ok(catalogue()))));
    for _ in 0..row {
        state.update(Action::Library(LibraryAction::SelectNext));
    }
    state
}

/// Metadata repair on the timeline's row `row`.
fn repairing(row: usize) -> AppState {
    let mut state = browsing(row);
    assert!(state.update(Action::RepairMetadata).is_empty());
    state
}

fn repair(state: &mut AppState, action: RepairAction) -> Vec<Effect> {
    state.update(Action::Repair(action))
}

fn written(effects: &[Effect]) -> Option<Edit> {
    effects.iter().find_map(|e| match e {
        Effect::Repair(RepairEffect::Apply(request)) => {
            assert_eq!(request.library, PathBuf::from(LIBRARY));
            assert_eq!(request.passphrase.expose(), "pw");
            Some(request.edit.clone())
        }
        _ => None,
    })
}

#[test]
fn the_context_is_the_assets_around_it_with_trash_left_out() {
    let catalogue = catalogue();
    // id 3 sits between 2 and 5 once 4 (in trash) is left out.
    assert_eq!(
        context(&catalogue, id(3), 1),
        Some((vec![id(2), id(3), id(5)], 1))
    );
    assert_eq!(
        context(&catalogue, id(1), SPAN),
        Some((vec![id(1), id(2), id(3), id(5)], 0))
    );
    // A trashed asset is still listed when it is the one repaired.
    assert_eq!(
        context(&catalogue, id(4), 1),
        Some((vec![id(3), id(4), id(5)], 1))
    );
    assert_eq!(context(&catalogue, Uuid::nil(), SPAN), None);
}

#[test]
fn delta_is_signed_in_the_two_largest_units() {
    let at = |t: &str| t.parse::<Timestamp>().ok();
    let from = at("2024-06-01T12:00:00Z");
    assert_eq!(delta(from, at("2024-06-03T15:30:00Z")), "+2d 3h");
    assert_eq!(delta(from, at("2024-06-01T10:15:00Z")), "-1h 45m");
    assert_eq!(delta(from, at("2024-06-01T12:07:00Z")), "+7m");
    assert_eq!(delta(from, at("2024-06-01T11:59:48Z")), "-12s");
    assert_eq!(delta(from, from), "same time");
    assert_eq!(delta(from, None), "?");
}

#[test]
fn place_text_names_the_datum_and_source() {
    assert_eq!(place_text(Some(PARIS)), "48.85840, 2.29450 (WGS-84, EXIF)");
    assert_eq!(place_text(None), "no location");
}

#[test]
fn it_opens_on_the_selected_asset_with_it_selected() {
    let state = repairing(1);
    assert_eq!(state.screen(), Screen::Repair);
    assert_eq!(state.repair().asset(), Some(id(2)));
    assert_eq!(state.repair().selected(&catalogue()), Some(id(2)));
}

#[test]
fn it_does_not_open_before_a_library_is() {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Library));
    state.update(Action::RepairMetadata);
    assert_eq!(state.screen(), Screen::Library);
}

#[test]
fn a_corrected_capture_time_is_written_from_the_prefilled_form() {
    let mut state = repairing(0);
    repair(&mut state, RepairAction::EditTime);
    assert!(state.is_editing());
    assert_eq!(
        state.repair().form().map(|f| f.value(0)),
        Some("2024-06-01T19:00:00Z")
    );
    clear_field(&mut state);
    type_text(&mut state, "2021-07-04T18:30:00+02:00");
    let effects = state.update(Action::Form(FormKey::Submit));
    assert_eq!(
        written(&effects),
        Some(Edit::SetCaptureTime {
            asset_id: id(1),
            capture: "2021-07-04T16:30:00Z".parse().expect("instant"),
        })
    );
    assert!(!state.is_editing());
    assert!(state.repair().is_working());
}

#[test]
fn a_time_without_an_offset_is_refused_and_the_form_stays() {
    let mut state = repairing(0);
    repair(&mut state, RepairAction::EditTime);
    clear_field(&mut state);
    type_text(&mut state, "2021-07-04 18:30");
    let effects = state.update(Action::Form(FormKey::Submit));
    assert!(effects.is_empty());
    assert!(state.is_editing());
    assert!(matches!(state.repair().status(), Status::Failed(r) if r.contains("RFC 3339")));
}

#[test]
fn a_typed_location_is_written_in_its_datum() {
    let mut state = repairing(0);
    repair(&mut state, RepairAction::EditLocation);
    assert_eq!(
        state.repair().form().map(|f| f.value(2)),
        Some("WGS-84"),
        "an asset with no location starts from WGS-84"
    );
    type_text(&mut state, "31.2304");
    state.update(Action::Form(FormKey::Next));
    type_text(&mut state, "121.4737");
    state.update(Action::Form(FormKey::Next));
    clear_field(&mut state);
    type_text(&mut state, "gcj02");
    let effects = state.update(Action::Form(FormKey::Submit));
    assert_eq!(
        written(&effects),
        Some(Edit::SetLocation {
            asset_id: id(1),
            lat: 31.2304,
            lon: 121.4737,
            datum: GpsDatum::Gcj02,
        })
    );
}

#[test]
fn a_location_form_prefills_the_current_fix_and_refuses_a_bad_number() {
    let mut state = repairing(1);
    repair(&mut state, RepairAction::EditLocation);
    let form = state.repair().form().expect("form");
    assert_eq!(
        (form.value(0), form.value(1), form.value(2)),
        ("48.8584", "2.2945", "WGS-84")
    );
    clear_field(&mut state);
    type_text(&mut state, "north");
    assert!(state.update(Action::Form(FormKey::Submit)).is_empty());
    assert!(matches!(state.repair().status(), Status::Failed(r) if r.starts_with("Latitude")));
    state.update(Action::Form(FormKey::Cancel));
    assert!(!state.is_editing());
}

#[test]
fn a_neighbours_location_is_copied_onto_the_asset() {
    let mut state = repairing(0);
    // The asset itself is selected: nothing to copy from.
    assert!(repair(&mut state, RepairAction::CopyLocation).is_empty());
    assert!(
        matches!(state.repair().status(), Status::Failed(r) if r.contains("Select a neighbour"))
    );

    repair(&mut state, RepairAction::SelectNext);
    let effects = repair(&mut state, RepairAction::CopyLocation);
    assert_eq!(
        written(&effects),
        Some(Edit::SetLocation {
            asset_id: id(1),
            lat: PARIS.lat,
            lon: PARIS.lon,
            datum: PARIS.datum,
        })
    );
}

#[test]
fn a_neighbour_with_no_location_has_none_to_copy() {
    let mut state = repairing(1);
    repair(&mut state, RepairAction::SelectNext);
    assert!(repair(&mut state, RepairAction::CopyLocation).is_empty());
    assert!(matches!(state.repair().status(), Status::Failed(r) if r.contains("no location")));
}

#[test]
fn the_selection_stays_inside_the_context() {
    let mut state = repairing(0);
    for _ in 0..10 {
        repair(&mut state, RepairAction::SelectNext);
    }
    assert_eq!(state.repair().selected(&catalogue()), Some(id(5)));
    for _ in 0..10 {
        repair(&mut state, RepairAction::SelectPrevious);
    }
    assert_eq!(state.repair().selected(&catalogue()), Some(id(1)));
}

#[test]
fn clearing_the_location_is_written_and_a_second_edit_waits() {
    let mut state = repairing(1);
    let effects = repair(&mut state, RepairAction::ClearLocation);
    assert_eq!(
        written(&effects),
        Some(Edit::ClearLocation { asset_id: id(2) })
    );
    assert!(repair(&mut state, RepairAction::ClearLocation).is_empty());
}

#[test]
fn an_edit_while_the_label_editor_writes_is_refused_with_a_reason() {
    let state = repairing(1);
    let mut screen = state.repair().clone();
    assert!(
        screen
            .update(RepairAction::ClearLocation, state.library(), true)
            .is_empty()
    );
    assert!(matches!(screen.status(), Status::Failed(r) if r.contains("Another edit")));
}

#[test]
fn a_written_correction_moves_the_asset_and_the_browser_follows_it() {
    let mut state = repairing(0);
    repair(&mut state, RepairAction::EditTime);
    // The correction makes id 1 the oldest asset: it moves to the end of the timeline.
    let mut assets = catalogue().assets().to_vec();
    let mut moved = assets.remove(0);
    moved.captured = "2020-01-01T00:00:00Z".parse().ok();
    moved.capture_text = "2020-01-01T00:00:00Z".to_owned();
    moved.capture_imported = Some("2024-06-01T19:00:00Z".to_owned());
    assets.push(moved);
    let edited = Edited {
        library: PathBuf::from(LIBRARY),
        summary: "Capture time now 2020-01-01T00:00:00Z.".to_owned(),
        catalogue: Catalogue::new(assets, Vec::new(), Vec::new()),
    };
    repair(&mut state, RepairAction::Applied(Ok(edited)));
    assert_eq!(
        state.repair().status(),
        &Status::Done("Capture time now 2020-01-01T00:00:00Z.".to_owned())
    );
    assert_eq!(state.library().selected_asset(), Some(id(1)));
    let frame = draw(&state);
    assert!(
        frame.contains("  as imported:    2024-06-01T19:00:00Z"),
        "{frame}"
    );
}

#[test]
fn a_failed_correction_reports_why() {
    let mut state = repairing(0);
    repair(&mut state, RepairAction::ClearLocation);
    let failed = RepairEffect::Apply(EditRequest {
        library: PathBuf::from(LIBRARY),
        passphrase: crate::tui::form::Secret::new("pw"),
        edit: Edit::ClearLocation { asset_id: id(1) },
    })
    .failed("Failed to open signed workspace: locked".to_owned());
    repair(&mut state, failed);
    assert!(matches!(state.repair().status(), Status::Failed(r) if r.contains("locked")));
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 24)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw");
    frame_text(terminal.backend().buffer())
}

#[test]
fn the_frame_shows_the_asset_and_its_neighbours_in_time() {
    let mut state = repairing(1);
    repair(&mut state, RepairAction::SelectNext);
    let frame = draw(&state);
    assert!(frame.contains("Time and place"), "{frame}");
    assert!(
        frame.contains("Captured:         2024-06-01T18:00:00Z"),
        "{frame}"
    );
    assert!(
        frame.contains("Location:         48.85840, 2.29450 (WGS-84, EXIF)"),
        "{frame}"
    );
    assert!(frame.contains("● 2024-06-01T18:00:00Z"), "{frame}");
    assert!(frame.contains("this"), "{frame}");
    assert!(
        frame.contains("2024-06-01T19:00:00Z           +1h 0m"),
        "{frame}"
    );
    assert!(frame.contains("-1h 0m  jpg  no location"), "{frame}");
    assert!(
        !frame.contains("2024-06-01T16:00:00Z"),
        "the trashed asset is left out: {frame}"
    );
    assert!(frame.contains("t Correct the capture time"), "{frame}");
    assert!(frame.contains("c Copy the neighbour's location"), "{frame}");
}

#[test]
fn the_screen_says_where_to_start_when_nothing_is_open() {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Repair));
    assert!(draw(&state).contains("Open a library in the browser"));
}
