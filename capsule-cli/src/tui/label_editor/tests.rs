//! Label editor tests: opening it from the browser, every edit it asks the port to write, the
//! region draft, the snapshot an edit hands back, the half-block canvas and the frame.

use std::sync::Arc;

use capsule_core::sidecar::StackRole;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::library::{
    AlbumEntry, AssetEntry, LibraryAction, Outline, RegionEntry, StackEntry,
};
use crate::tui::state::{Action, AppState, Screen};
use crate::tui::view;

const ALBUM: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00a1);
const STACK: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00c1);
const ASSET: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_0101);
const OTHER: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_0102);
const REGION: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_0e01);
const LIBRARY: &str = "/photos/lib";

fn entry(asset_id: Uuid, captured: &str) -> AssetEntry {
    AssetEntry {
        asset_id,
        album_id: ALBUM,
        captured: captured.parse().ok(),
        ext: "jpg".to_owned(),
        original: Some(PathBuf::from(format!("/lib/media/{asset_id}.jpg"))),
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

/// One asset labelled `beach` with one labelled region, stacked with a second; one album.
fn catalogue() -> Catalogue {
    let mut asset = entry(ASSET, "2024-06-01T10:00:00Z");
    asset.labels = vec!["beach".to_owned()];
    asset.stack = Some((STACK, StackRole::Primary));
    asset.regions = vec![RegionEntry {
        region_id: REGION,
        outline: Outline(RegionShape::Rect {
            x: 0.1,
            y: 0.1,
            w: 0.3,
            h: 0.3,
        }),
        labels: vec!["Ada".to_owned()],
    }];
    let mut other = entry(OTHER, "2024-05-31T10:00:00Z");
    other.stack = Some((STACK, StackRole::Member));
    Catalogue::new(
        vec![asset, other],
        vec![AlbumEntry {
            album_id: ALBUM,
            name: "Imports".to_owned(),
            labels: Ok(vec!["trip".to_owned()]),
        }],
        vec![StackEntry {
            stack_id: STACK,
            members: vec![(ASSET, StackRole::Primary), (OTHER, StackRole::Member)],
            labels: Ok(Vec::new()),
        }],
    )
}

fn type_text(state: &mut AppState, text: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    for c in text.chars() {
        effects.extend(state.update(Action::Form(FormKey::Char(c))));
    }
    effects
}

/// The browser on `catalogue`, opened as `LIBRARY` with passphrase `pw`.
fn browsing() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Library));
    state.update(Action::Library(LibraryAction::Edit));
    type_text(&mut state, LIBRARY);
    state.update(Action::Form(FormKey::Next));
    type_text(&mut state, "pw");
    state.update(Action::Form(FormKey::Submit));
    state.update(Action::Library(LibraryAction::Opened(Ok(catalogue()))));
    state
}

/// The label editor on the browser's first timeline row, the stacked asset.
fn editing_asset() -> (AppState, Vec<Effect>) {
    let mut state = browsing();
    let effects = state.update(Action::EditLabels);
    (state, effects)
}

fn label(state: &mut AppState, action: LabelAction) -> Vec<Effect> {
    state.update(Action::Labels(action))
}

fn applied(effects: &[Effect]) -> Option<&EditRequest> {
    effects.iter().find_map(|e| match e {
        Effect::Labels(LabelEffect::Apply(request)) => Some(request),
        _ => None,
    })
}

#[test]
fn opening_on_an_asset_decodes_its_preview_for_drawing_regions() {
    let (state, effects) = editing_asset();
    assert_eq!(state.screen(), Screen::Labels);
    assert_eq!(state.labels().subject(), Some(LabelTarget::Asset(ASSET)));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Labels(LabelEffect::Preview(PreviewRequest {
            asset_id: ASSET,
            ..
        }))]
    ));
    assert_eq!(
        state.labels().preview().map(|(id, l)| (*id, l.clone())),
        Some((ASSET, Loadable::Loading))
    );
}

#[test]
fn opening_on_an_album_or_stack_labels_it_and_decodes_nothing() {
    let mut state = browsing();
    state.update(Action::Library(LibraryAction::NextView));
    assert!(state.update(Action::EditLabels).is_empty());
    assert_eq!(state.labels().subject(), Some(LabelTarget::Album(ALBUM)));
    let catalogue = catalogue();
    assert_eq!(
        state.labels().rows(&catalogue),
        vec![EditorRow::Label("trip".to_owned())]
    );

    state.update(Action::Back);
    state.update(Action::Library(LibraryAction::NextView));
    assert!(state.update(Action::EditLabels).is_empty());
    assert_eq!(state.labels().subject(), Some(LabelTarget::Stack(STACK)));
    assert!(state.labels().rows(&catalogue).is_empty());
}

#[test]
fn the_editor_does_not_open_before_a_library_is() {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Library));
    assert!(state.update(Action::EditLabels).is_empty());
    assert_eq!(state.screen(), Screen::Library);
}

#[test]
fn rows_list_the_labels_then_each_region_and_its_labels() {
    let (state, _) = editing_asset();
    assert_eq!(
        state.labels().rows(&catalogue()),
        vec![
            EditorRow::Label("beach".to_owned()),
            EditorRow::Region(REGION),
            EditorRow::RegionLabel(REGION, "Ada".to_owned()),
        ]
    );
}

#[test]
fn a_typed_label_is_written_to_the_asset_with_the_browsers_credentials() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Add);
    assert!(state.is_editing());
    type_text(&mut state, "sunset");
    let effects = state.update(Action::Form(FormKey::Submit));
    let request = applied(&effects).expect("an apply");
    assert_eq!(request.library, PathBuf::from(LIBRARY));
    assert_eq!(request.passphrase.expose(), "pw");
    assert_eq!(
        request.edit,
        Edit::AddLabel {
            target: LabelTarget::Asset(ASSET),
            label: "sunset".to_owned(),
        }
    );
    assert!(!state.is_editing());
    assert!(state.labels().is_working());
}

#[test]
fn an_empty_label_is_refused_before_anything_is_written() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Add);
    let effects = state.update(Action::Form(FormKey::Submit));
    assert!(effects.is_empty());
    assert!(matches!(state.labels().status(), Status::Failed(_)));
}

#[test]
fn a_label_typed_on_a_region_row_labels_the_region() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Nudge(Nudge::Down));
    label(&mut state, LabelAction::Add);
    type_text(&mut state, "face");
    let effects = state.update(Action::Form(FormKey::Submit));
    assert_eq!(
        applied(&effects).map(|r| r.edit.clone()),
        Some(Edit::AddLabel {
            target: LabelTarget::Region {
                asset_id: ASSET,
                region_id: REGION,
            },
            label: "face".to_owned(),
        })
    );
}

#[test]
fn removing_a_label_row_removes_that_label_from_its_owner() {
    let (mut state, _) = editing_asset();
    let effects = label(&mut state, LabelAction::Remove);
    assert_eq!(
        applied(&effects).map(|r| r.edit.clone()),
        Some(Edit::RemoveLabel {
            target: LabelTarget::Asset(ASSET),
            label: "beach".to_owned(),
        })
    );

    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Nudge(Nudge::Down));
    label(&mut state, LabelAction::Nudge(Nudge::Down));
    let effects = label(&mut state, LabelAction::Remove);
    assert_eq!(
        applied(&effects).map(|r| r.edit.clone()),
        Some(Edit::RemoveLabel {
            target: LabelTarget::Region {
                asset_id: ASSET,
                region_id: REGION,
            },
            label: "Ada".to_owned(),
        })
    );
}

#[test]
fn deleting_a_region_asks_first() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Nudge(Nudge::Down));
    assert!(label(&mut state, LabelAction::Remove).is_empty());
    assert!(state.is_confirming());
    assert!(matches!(state.labels().status(), Status::Asking(_)));

    assert!(state.update(Action::Answer(Answer::No)).is_empty());
    assert!(!state.is_confirming());

    label(&mut state, LabelAction::Remove);
    let effects = state.update(Action::Answer(Answer::Yes));
    assert_eq!(
        applied(&effects).map(|r| r.edit.clone()),
        Some(Edit::DeleteRegion {
            asset_id: ASSET,
            region_id: REGION,
        })
    );
}

#[test]
fn a_drawn_circle_moves_resizes_and_is_written_with_its_label() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::DrawCircle);
    assert_eq!(state.labels().draft(), Some(Draft::CIRCLE));
    label(&mut state, LabelAction::Nudge(Nudge::Right));
    label(&mut state, LabelAction::Nudge(Nudge::Up));
    label(
        &mut state,
        LabelAction::Resize {
            width: 1,
            height: 1,
        },
    );
    assert_eq!(
        state.labels().draft(),
        Some(Draft::Circle {
            cx: 52,
            cy: 48,
            r: 18
        })
    );
    // Up and Down move the draft, not the selection.
    assert_eq!(state.labels().cursor(&catalogue()), 0);

    label(&mut state, LabelAction::Finish);
    assert!(state.is_editing());
    type_text(&mut state, "Ada");
    let effects = state.update(Action::Form(FormKey::Submit));
    assert_eq!(
        applied(&effects).map(|r| r.edit.clone()),
        Some(Edit::AddRegion {
            asset_id: ASSET,
            shape: RegionShape::Circle {
                cx: 0.52,
                cy: 0.48,
                r: 0.18,
            },
            labels: vec!["Ada".to_owned()],
        })
    );
    assert_eq!(state.labels().draft(), None);
}

#[test]
fn a_region_written_with_no_label_is_left_unlabelled() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::DrawRect);
    label(&mut state, LabelAction::Finish);
    let effects = state.update(Action::Form(FormKey::Submit));
    assert!(matches!(
        applied(&effects).map(|r| r.edit.clone()),
        Some(Edit::AddRegion { labels, .. }) if labels.is_empty()
    ));
}

#[test]
fn esc_drops_the_draft_before_it_leaves_the_screen() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::DrawRect);
    state.update(Action::Back);
    assert_eq!(state.screen(), Screen::Labels);
    assert_eq!(state.labels().draft(), None);
    state.update(Action::Back);
    assert_eq!(state.screen(), Screen::Library);
}

#[test]
fn an_album_or_stack_has_no_regions_to_draw() {
    let mut state = browsing();
    state.update(Action::Library(LibraryAction::NextView));
    state.update(Action::EditLabels);
    label(&mut state, LabelAction::DrawCircle);
    assert_eq!(state.labels().draft(), None);
}

#[test]
fn a_draft_stays_inside_the_image_however_far_it_is_pushed() {
    let mut draft = Draft::RECT;
    for _ in 0..100 {
        draft = draft.moved(STEP, STEP).resized(STEP, STEP);
    }
    assert_eq!(
        draft,
        Draft::Rect {
            x: 52,
            y: 52,
            w: 48,
            h: 48
        },
        "pushed into the corner, it grows only until it meets the edges"
    );
    for _ in 0..100 {
        draft = draft.moved(-STEP, -STEP).resized(-STEP, -STEP);
    }
    assert_eq!(
        draft,
        Draft::Rect {
            x: 0,
            y: 0,
            w: MIN_EXTENT,
            h: MIN_EXTENT
        }
    );
    let mut circle = Draft::CIRCLE;
    for _ in 0..100 {
        circle = circle.moved(-STEP, STEP).resized(-STEP, 0);
    }
    assert_eq!(
        circle,
        Draft::Circle {
            cx: 0,
            cy: 100,
            r: MIN_EXTENT
        }
    );
}

#[test]
fn every_rectangle_a_draft_can_reach_passes_the_workspace_check() {
    for x in 0..=98_i16 {
        for w in MIN_EXTENT..=(100 - x) {
            let shape = Draft::Rect { x, y: x, w, h: w }.shape();
            assert_eq!(shape.validate(), Ok(()), "x {x} w {w}: {shape:?}");
        }
    }
}

#[test]
fn a_second_edit_waits_for_the_first() {
    let (mut state, _) = editing_asset();
    assert!(applied(&label(&mut state, LabelAction::Remove)).is_some());
    label(&mut state, LabelAction::Nudge(Nudge::Down));
    label(&mut state, LabelAction::Nudge(Nudge::Down));
    assert!(label(&mut state, LabelAction::Remove).is_empty());
}

#[test]
fn an_edit_while_the_repair_screen_writes_is_refused_with_a_reason() {
    let (state, _) = editing_asset();
    let mut library = state.library().clone();
    let mut editor = state.labels().clone();
    let effects = editor.update(LabelAction::Remove, &library, true);
    assert!(effects.is_empty());
    assert!(matches!(editor.status(), Status::Failed(r) if r.contains("Another edit")));
    // Nothing is open after a failed reopen: an edit has nowhere to go.
    library.update(LibraryAction::Opened(Err("wrong passphrase".to_owned())));
    let mut editor = state.labels().clone();
    assert!(
        editor
            .update(LabelAction::Remove, &library, false)
            .is_empty()
    );
}

#[test]
fn a_written_edit_replaces_the_browsers_snapshot_and_says_what_it_did() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Remove);
    let mut after = catalogue().assets().to_vec();
    after[0].labels.clear();
    let edited = Edited {
        library: PathBuf::from(LIBRARY),
        summary: "Label removed. Labels now: (none).".to_owned(),
        catalogue: Catalogue::new(
            after,
            catalogue().albums().to_vec(),
            catalogue().stacks().to_vec(),
        ),
    };
    label(&mut state, LabelAction::Applied(Ok(edited)));
    assert_eq!(
        state.labels().status(),
        &Status::Done("Label removed. Labels now: (none).".to_owned())
    );
    let now = state.library().ready().expect("open");
    assert!(now.asset(ASSET).expect("asset").labels.is_empty());
    assert_eq!(
        state.labels().rows(now).first(),
        Some(&EditorRow::Region(REGION))
    );
}

#[test]
fn a_snapshot_of_another_library_is_dropped() {
    let (mut state, _) = editing_asset();
    let before = state.library().ready().cloned();
    let edited = Edited {
        library: PathBuf::from("/elsewhere"),
        summary: "Label added.".to_owned(),
        catalogue: Catalogue::default(),
    };
    label(&mut state, LabelAction::Applied(Ok(edited)));
    assert_eq!(state.library().ready().cloned(), before);
}

#[test]
fn a_failed_edit_reports_why() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::Remove);
    let failed = LabelEffect::Apply(EditRequest {
        library: PathBuf::from(LIBRARY),
        passphrase: Secret::new("pw"),
        edit: Edit::ClearLocation { asset_id: ASSET },
    })
    .failed("the task panicked".to_owned());
    label(&mut state, failed);
    assert_eq!(
        state.labels().status(),
        &Status::Failed("the task panicked".to_owned())
    );
    assert!(!state.labels().is_working());
}

#[test]
fn compose_draws_an_outline_on_the_cells_a_region_covers() {
    // A blank 10×5 pane is a 10×10 pixel grid: a rectangle over its left half.
    let lines = compose(
        None,
        10,
        5,
        &[(
            RegionShape::Rect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            Highlight::Draft,
        )],
    );
    assert_eq!(lines.len(), 5);
    let fg = |row: usize, col: usize| lines[row].spans[col].style.fg;
    let bg = |row: usize, col: usize| lines[row].spans[col].style.bg;
    // The left and right edges, top to bottom.
    for (row, line) in lines.iter().enumerate() {
        assert_eq!(fg(row, 0), Some(Color::Magenta), "row {row}");
        assert_eq!(fg(row, 4), Some(Color::Magenta), "row {row}");
        assert_eq!(line.spans[5].content, " ", "row {row}: outside");
    }
    // Inside, between the edges: the top pixel row of the first cell row is the top edge.
    assert_eq!(fg(0, 2), Some(Color::Magenta));
    assert_eq!(bg(0, 2), Some(Color::Reset));
    assert_eq!(lines[2].spans[2].content, " ", "inside, off the outline");
    assert_eq!(bg(4, 2), Some(Color::Magenta), "the bottom edge");
}

#[test]
fn compose_fits_the_image_and_samples_its_pixels() {
    // A 2×1 image (red, blue) on a 4×3 pane (4×6 pixels): fitted to 4×2, centred vertically.
    let image = PreviewImage {
        width: 2,
        height: 1,
        rgba: vec![255, 0, 0, 255, 0, 0, 255, 255].into(),
    };
    let lines = compose(Some(&image), 4, 3, &[]);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2].spans[0].content, " ", "below the image is blank");
    assert_eq!(lines[0].spans[0].content, " ", "above the image is blank");
    let red = Color::Rgb(255, 0, 0);
    let blue = Color::Rgb(0, 0, 255);
    let cell = &lines[1].spans;
    // Pixel rows 2 and 3 hold the image: cell row 1, top and bottom halves.
    assert_eq!((cell[0].style.fg, cell[0].style.bg), (Some(red), Some(red)));
    assert_eq!(
        (cell[3].style.fg, cell[3].style.bg),
        (Some(blue), Some(blue))
    );
    assert!(compose(Some(&image), 0, 4, &[]).is_empty());
}

#[test]
fn a_circle_is_round_on_a_wide_image() {
    // A 20×10 pixel grid; a circle of radius 0.5 of the shorter side touches the top and the
    // bottom and spans only the middle half horizontally.
    let lines = compose(
        None,
        20,
        5,
        &[(
            RegionShape::Circle {
                cx: 0.5,
                cy: 0.5,
                r: 0.5,
            },
            Highlight::Region,
        )],
    );
    let drawn = |col: usize| {
        lines.iter().any(|l| {
            l.spans[col].style.fg == Some(Color::Cyan) || l.spans[col].style.bg == Some(Color::Cyan)
        })
    };
    assert!(drawn(10));
    assert!(!drawn(2), "a circle, not an ellipse stretched to the width");
    assert!(!drawn(17));
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw");
    frame_text(terminal.backend().buffer())
}

#[test]
fn the_frame_lists_labels_and_regions_and_offers_the_editing_keys() {
    let (mut state, _) = editing_asset();
    label(
        &mut state,
        LabelAction::PreviewLoaded {
            asset_id: ASSET,
            result: Err("this build has no decoder for JPEG stills".to_owned()),
        },
    );
    let frame = draw(&state);
    assert!(frame.contains("Labels"), "{frame}");
    assert!(
        frame.contains("Asset 00000101 · 2024-06-01T10:00:00Z · jpg"),
        "{frame}"
    );
    assert!(frame.contains("› label   beach"), "{frame}");
    assert!(frame.contains("region  00000e01 rectangle"), "{frame}");
    assert!(frame.contains("Ada"), "{frame}");
    assert!(
        frame.contains("No preview: this build has no decoder"),
        "{frame}"
    );
    assert!(frame.contains("a Add a label"), "{frame}");
    assert!(frame.contains("r Draw a rectangle"), "{frame}");
}

#[test]
fn the_frame_says_how_to_draw_while_a_region_is_drawn() {
    let (mut state, _) = editing_asset();
    label(&mut state, LabelAction::DrawRect);
    let frame = draw(&state);
    assert!(frame.contains("Drawing: arrows move it"), "{frame}");
    label(&mut state, LabelAction::Finish);
    let frame = draw(&state);
    assert!(frame.contains("The new region's label"), "{frame}");
}

#[test]
fn the_editor_says_where_to_start_when_nothing_is_open() {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Labels));
    let frame = draw(&state);
    assert!(frame.contains("Open a library in the browser"), "{frame}");
}

#[test]
fn the_port_reports_an_edit_it_cannot_make() {
    struct NoPorts;
    impl crate::tui::ports::Ports for NoPorts {
        async fn session_summary(&self) -> Result<crate::tui::state::SessionSummary, String> {
            Ok(crate::tui::state::SessionSummary::SignedOut)
        }
    }
    let ports = Arc::new(NoPorts);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let action = runtime.block_on(crate::tui::ports::perform(
        ports.as_ref(),
        Effect::Labels(LabelEffect::Apply(EditRequest {
            library: PathBuf::from(LIBRARY),
            passphrase: Secret::new("pw"),
            edit: Edit::ClearLocation { asset_id: ASSET },
        })),
    ));
    assert_eq!(
        action,
        Action::Labels(LabelAction::Applied(Err(
            crate::tui::ports::NOT_OFFERED.to_owned()
        )))
    );
}
