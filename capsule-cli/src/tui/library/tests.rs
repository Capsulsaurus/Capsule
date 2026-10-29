//! Library browser tests: the open form, navigation over a hand-built catalogue, the
//! one-at-a-time preview, the preview decode, and the frames it draws.

use std::sync::atomic::{AtomicUsize, Ordering};

use capsule_core::media::{DecodedImage, MediaError, MediaMetadata, RawshiftDecoder};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::{Driver, frame_text};
use crate::tui::ports::Ports;
use crate::tui::state::{Action, AppState, Screen, SessionSummary};
use crate::tui::view;

const ALBUM: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00a1);
const TRIPS: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00a2);
const STACK: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00c1);
const DEVICE: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00d1);

const fn asset_id(n: u128) -> Uuid {
    Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_0100 + n)
}

fn asset(n: u128, captured: &str, album_id: Uuid) -> AssetEntry {
    let id = asset_id(n);
    AssetEntry {
        asset_id: id,
        album_id,
        captured: captured.parse().ok(),
        ext: "jpg".to_owned(),
        original: Some(PathBuf::from(format!("/lib/media/{n}.jpg"))),
        hidden: false,
        in_trash: false,
        stack: None,
        rows: vec![
            ("Album:", album_id.to_string()),
            ("Captured:", captured.to_owned()),
            ("User tags:", "beach, sunset".to_owned()),
        ],
        history: vec![
            HistoryEntry {
                timestamp: "2024-05-02T08:00:00Z".to_owned(),
                action: "create",
                device: DEVICE,
                client_version: "capsule-cli/0.1.0".to_owned(),
                retention_until: None,
            },
            HistoryEntry {
                timestamp: "2024-05-03T09:30:00Z".to_owned(),
                action: "metadata-update",
                device: DEVICE,
                client_version: "capsule-cli/0.1.0".to_owned(),
                retention_until: None,
            },
        ],
        labels: vec!["beach".to_owned(), "sunset".to_owned()],
        regions: Vec::new(),
        capture_text: captured.to_owned(),
        capture_imported: None,
        location: None,
        location_imported: None,
        location_edited: false,
    }
}

/// Five assets: two stacked (1 primary, 2 member), one hidden, one in trash, one in a second
/// album. Newest first, as `collect` orders them.
fn assets() -> Vec<AssetEntry> {
    let mut primary = asset(1, "2024-06-01T10:00:00Z", ALBUM);
    primary.stack = Some((STACK, StackRole::Primary));
    let mut member = asset(2, "2024-05-31T10:00:00Z", ALBUM);
    member.stack = Some((STACK, StackRole::Member));
    let mut hidden = asset(3, "2024-05-30T10:00:00Z", ALBUM);
    hidden.hidden = true;
    let mut trashed = asset(4, "2024-05-29T10:00:00Z", ALBUM);
    trashed.in_trash = true;
    let trip = asset(5, "2023-12-24T18:30:00Z", TRIPS);
    vec![primary, member, hidden, trashed, trip]
}

/// The two albums `assets()` are in.
fn albums() -> Vec<AlbumEntry> {
    vec![
        AlbumEntry {
            album_id: ALBUM,
            name: "Imports".to_owned(),
            labels: Ok(vec!["family".to_owned()]),
        },
        AlbumEntry {
            album_id: TRIPS,
            name: "Trips".to_owned(),
            labels: Err("the label record does not verify".to_owned()),
        },
    ]
}

/// The one stack of `assets()`.
fn stacks() -> Vec<StackEntry> {
    vec![StackEntry {
        stack_id: STACK,
        members: vec![
            (asset_id(1), StackRole::Primary),
            (asset_id(2), StackRole::Member),
        ],
        labels: Ok(vec!["best".to_owned()]),
    }]
}

fn catalogue() -> Catalogue {
    Catalogue::new(assets(), albums(), stacks())
}

fn preview_of(n: u128) -> Effect {
    Effect::Library(LibraryEffect::Preview(PreviewRequest {
        asset_id: asset_id(n),
        original: PathBuf::from(format!("/lib/media/{n}.jpg")),
        ext: "jpg".to_owned(),
    }))
}

fn image(width: u32, height: u32) -> PreviewImage {
    PreviewImage {
        width,
        height,
        rgba: vec![200; (width * height * 4) as usize].into(),
    }
}

/// A browser with `catalogue()` open and the first preview answered with `answer`.
fn opened() -> LibraryScreen {
    let mut screen = LibraryScreen::default();
    let effects = screen.update(LibraryAction::Opened(Ok(catalogue())));
    assert_eq!(effects, vec![preview_of(1)]);
    screen
}

fn answer(
    screen: &mut LibraryScreen,
    n: u128,
    result: Result<PreviewImage, String>,
) -> Vec<Effect> {
    screen.update(LibraryAction::PreviewLoaded {
        asset_id: asset_id(n),
        result,
    })
}

// ── The open form ─────────────────────────────────────────────────────────

fn submitted(library: &str, passphrase: &str) -> (LibraryScreen, Vec<Effect>) {
    let mut screen = LibraryScreen::default();
    screen.update(LibraryAction::Edit);
    screen.form.set(LIBRARY, library);
    screen.form.set(PASSPHRASE, passphrase);
    let effects = screen.on_form_key(FormKey::Submit);
    (screen, effects)
}

#[test]
fn submitting_the_form_opens_the_library_and_forgets_the_passphrase() {
    let (screen, effects) = submitted(" /photos/lib ", "pass phrase");
    assert_eq!(
        effects,
        vec![Effect::Library(LibraryEffect::Open(OpenRequest {
            library: PathBuf::from("/photos/lib"),
            passphrase: Secret::new("pass phrase"),
        }))]
    );
    assert_eq!(screen.catalogue(), Some(&Loadable::Loading));
    assert!(!screen.is_editing());
    assert!(screen.form().secret(PASSPHRASE).is_empty());
}

#[test]
fn the_form_is_refused_without_a_library_or_a_passphrase() {
    for (library, passphrase) in [("", "pw"), ("/lib", ""), ("   ", "pw")] {
        let (screen, effects) = submitted(library, passphrase);
        assert!(effects.is_empty());
        assert_eq!(
            screen.invalid(),
            Some("Enter the library and its passphrase.")
        );
        assert!(screen.is_editing());
        assert_eq!(screen.catalogue(), None);
    }
}

#[test]
fn edit_waits_while_a_library_opens_and_cancel_stops_editing() {
    let (mut screen, _) = submitted("/lib", "pw");
    screen.update(LibraryAction::Edit);
    assert!(!screen.is_editing());
    screen.update(LibraryAction::Opened(Err("wrong passphrase".to_owned())));
    assert_eq!(
        screen.catalogue(),
        Some(&Loadable::Failed("wrong passphrase".to_owned()))
    );
    screen.update(LibraryAction::Edit);
    assert!(screen.is_editing());
    screen.on_form_key(FormKey::Cancel);
    assert!(!screen.is_editing());
    assert!(screen.on_form_key(FormKey::Submit).is_empty());
}

#[test]
fn the_passphrase_never_reaches_debug() {
    let mut screen = LibraryScreen::default();
    screen.update(LibraryAction::Edit);
    screen.form.set(LIBRARY, "/lib");
    screen.form.set(PASSPHRASE, "hunter2");
    let printed_form = format!("{screen:?}");
    let effects = screen.on_form_key(FormKey::Submit);
    let printed = format!("{printed_form} {effects:?}");
    assert!(!printed.contains("hunter2"), "{printed}");
}

#[test]
fn a_failed_effect_reports_its_outcome_failing() {
    let open = LibraryEffect::Open(OpenRequest {
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
    });
    assert_eq!(
        open.failed("panicked".to_owned()),
        LibraryAction::Opened(Err("panicked".to_owned()))
    );
    let Effect::Library(preview) = preview_of(3) else {
        unreachable!()
    };
    assert_eq!(
        preview.failed("panicked".to_owned()),
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(3),
            result: Err("panicked".to_owned()),
        }
    );
}

// ── Navigation ────────────────────────────────────────────────────────────

#[test]
fn moving_keys_do_nothing_before_a_library_is_open() {
    let mut screen = LibraryScreen::default();
    for action in [
        LibraryAction::NextView,
        LibraryAction::SelectNext,
        LibraryAction::SelectPrevious,
        LibraryAction::Enter,
        LibraryAction::Leave,
    ] {
        assert!(screen.update(action).is_empty());
    }
    assert_eq!(screen, LibraryScreen::default());
}

#[test]
fn the_timeline_shows_a_stack_as_its_primary_and_leaves_out_hidden_and_trash() {
    let screen = opened();
    assert_eq!(screen.view(), View::Timeline);
    assert_eq!(
        screen.rows(),
        vec![Row::Asset(asset_id(1)), Row::Asset(asset_id(5))]
    );
    assert_eq!(screen.selected_asset(), Some(asset_id(1)));
}

#[test]
fn the_selection_stops_at_both_ends() {
    let mut screen = opened();
    answer(&mut screen, 1, Ok(image(4, 4)));
    screen.update(LibraryAction::SelectPrevious);
    assert_eq!(screen.cursor(), 0);
    screen.update(LibraryAction::SelectNext);
    screen.update(LibraryAction::SelectNext);
    screen.update(LibraryAction::SelectNext);
    assert_eq!(screen.cursor(), 1);
    assert_eq!(screen.selected(), Some(Row::Asset(asset_id(5))));
}

#[test]
fn one_preview_decodes_at_a_time_and_the_latest_selection_wins() {
    let mut screen = opened();
    // The first decode is running: moving on asks for nothing yet.
    assert!(screen.update(LibraryAction::SelectNext).is_empty());
    assert_eq!(screen.preview(), Some(&(asset_id(1), Loadable::Loading)));
    // Its answer is kept for the asset it is of, and the selection's preview is asked next.
    assert_eq!(answer(&mut screen, 1, Ok(image(4, 4))), vec![preview_of(5)]);
    assert_eq!(screen.preview(), Some(&(asset_id(5), Loadable::Loading)));
    // Back to the first: asked again, since only the latest preview is held.
    assert!(screen.update(LibraryAction::SelectPrevious).is_empty());
    assert_eq!(
        answer(&mut screen, 5, Err("no codec".to_owned())),
        vec![preview_of(1)]
    );
}

#[test]
fn a_shown_preview_is_not_asked_for_again() {
    let mut screen = opened();
    assert!(answer(&mut screen, 1, Ok(image(4, 4))).is_empty());
    assert_eq!(
        screen.preview(),
        Some(&(asset_id(1), Loadable::Ready(image(4, 4))))
    );
    // Moving within a one-asset step back onto the same asset asks for nothing.
    assert!(screen.update(LibraryAction::SelectPrevious).is_empty());
}

#[test]
fn an_asset_with_no_original_on_this_device_says_so_without_asking() {
    let mut assets = assets();
    assets[0].original = None;
    let catalogue = Catalogue::new(assets, albums(), stacks());
    let mut screen = LibraryScreen::default();
    assert!(
        screen
            .update(LibraryAction::Opened(Ok(catalogue)))
            .is_empty()
    );
    assert_eq!(
        screen.preview(),
        Some(&(
            asset_id(1),
            Loadable::Failed("the original is not on this device".to_owned())
        ))
    );
}

#[test]
fn albums_open_onto_their_assets_and_left_leaves() {
    let mut screen = opened();
    answer(&mut screen, 1, Ok(image(4, 4)));
    screen.update(LibraryAction::NextView);
    assert_eq!(screen.view(), View::Albums);
    assert_eq!(screen.rows(), vec![Row::Album(ALBUM), Row::Album(TRIPS)]);
    assert_eq!(screen.selected_asset(), None, "an album has no preview");

    // The album lists everything but trash, hidden and stacked members included.
    assert!(screen.update(LibraryAction::Enter).is_empty());
    assert_eq!(screen.group(), Some(Group::Album(ALBUM)));
    assert_eq!(
        screen.rows(),
        vec![
            Row::Asset(asset_id(1)),
            Row::Asset(asset_id(2)),
            Row::Asset(asset_id(3)),
        ]
    );
    assert_eq!(
        screen.update(LibraryAction::SelectNext),
        vec![preview_of(2)]
    );
    // Enter on an asset does nothing.
    assert!(screen.update(LibraryAction::Enter).is_empty());
    assert_eq!(screen.group(), Some(Group::Album(ALBUM)));

    screen.update(LibraryAction::Leave);
    assert_eq!(screen.group(), None);
    assert_eq!(screen.selected(), Some(Row::Album(ALBUM)));
    // Leaving with nothing open does nothing.
    assert!(screen.update(LibraryAction::Leave).is_empty());
}

#[test]
fn a_stack_previews_its_primary_and_opens_onto_its_members() {
    let mut screen = opened();
    answer(&mut screen, 1, Ok(image(4, 4)));
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::NextView);
    assert_eq!(screen.view(), View::Stacks);
    assert_eq!(screen.rows(), vec![Row::Stack(STACK)]);
    assert_eq!(screen.selected_asset(), Some(asset_id(1)));
    screen.update(LibraryAction::Enter);
    assert_eq!(screen.group(), Some(Group::Stack(STACK)));
    assert_eq!(
        screen.rows(),
        vec![Row::Asset(asset_id(1)), Row::Asset(asset_id(2))]
    );
    // The next view leaves the stack and rounds back to the timeline.
    screen.update(LibraryAction::NextView);
    assert_eq!(screen.view(), View::Timeline);
    assert_eq!(screen.group(), None);
}

#[test]
fn each_view_keeps_its_own_selection() {
    let mut screen = opened();
    answer(&mut screen, 1, Ok(image(4, 4)));
    screen.update(LibraryAction::SelectNext);
    screen.update(LibraryAction::NextView);
    assert_eq!(screen.cursor(), 0);
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::NextView);
    assert_eq!(screen.view(), View::Timeline);
    assert_eq!(screen.cursor(), 1);
}

#[test]
fn reopening_resets_the_views_and_ignores_a_stale_preview() {
    let mut screen = opened();
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::Enter);
    // The first decode is still running when the library is opened again.
    assert!(
        screen
            .update(LibraryAction::Opened(Ok(catalogue())))
            .is_empty()
    );
    assert_eq!(screen.view(), View::Timeline);
    assert_eq!(screen.group(), None);
    assert_eq!(screen.preview(), None);
    // The stale answer lands on nothing and asks for the current selection.
    assert_eq!(answer(&mut screen, 1, Ok(image(4, 4))), vec![preview_of(1)]);
}

// ── A snapshot after an edit ──────────────────────────────────────────────

/// A browser opened through the form on `/lib`, with `catalogue()` loaded.
fn opened_on_lib() -> LibraryScreen {
    let (mut screen, _) = submitted("/lib", "pw");
    screen.update(LibraryAction::Opened(Ok(catalogue())));
    screen
}

/// `catalogue()` without asset 5 and its album, Trips, and with the stack dissolved.
fn without_trips_or_stack() -> Catalogue {
    let mut assets = assets();
    assets.pop();
    for asset in &mut assets {
        asset.stack = None;
    }
    Catalogue::new(assets, albums()[..1].to_vec(), Vec::new())
}

#[test]
fn a_replaced_catalogue_leaves_an_album_or_stack_that_is_gone() {
    let mut screen = opened_on_lib();
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::SelectNext);
    screen.update(LibraryAction::Enter);
    assert_eq!(screen.group(), Some(Group::Album(TRIPS)));
    screen.replace_catalogue(Path::new("/lib"), without_trips_or_stack());
    assert_eq!(screen.group(), None);
    // The Trips row is gone too: the albums cursor is clamped onto the last album left.
    assert_eq!(screen.selected(), Some(Row::Album(ALBUM)));

    let mut screen = opened_on_lib();
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::Enter);
    assert_eq!(screen.group(), Some(Group::Stack(STACK)));
    screen.replace_catalogue(Path::new("/lib"), without_trips_or_stack());
    assert_eq!(screen.group(), None);
    assert_eq!(screen.selected(), None, "no stacks are left");
}

#[test]
fn a_replaced_catalogue_clamps_the_cursor_when_the_selected_row_is_gone() {
    let mut screen = opened_on_lib();
    answer(&mut screen, 1, Ok(image(4, 4)));
    screen.update(LibraryAction::SelectNext);
    assert_eq!(screen.selected(), Some(Row::Asset(asset_id(5))));
    let mut assets = assets();
    assets.pop();
    screen.replace_catalogue(
        Path::new("/lib"),
        Catalogue::new(assets, albums(), stacks()),
    );
    // Asset 5 is gone and the timeline is the stack alone: the cursor is clamped onto it
    // rather than left past the end.
    assert_eq!(screen.rows(), vec![Row::Asset(asset_id(1))]);
    assert_eq!(screen.cursor(), 0);
    assert_eq!(screen.selected(), Some(Row::Asset(asset_id(1))));

    // A snapshot before any library is open is dropped.
    let mut closed = LibraryScreen::default();
    assert!(
        closed
            .replace_catalogue(Path::new("/lib"), catalogue())
            .is_empty()
    );
    assert_eq!(closed.catalogue(), None);
}

// ── A large library ───────────────────────────────────────────────────────

const LARGE: u128 = 50_000;

/// `LARGE` assets in one album, every tenth stacked with the next as its member.
fn large_catalogue() -> Catalogue {
    let mut assets = Vec::new();
    let mut stacks = Vec::new();
    for n in 0..LARGE {
        let mut entry = asset(n, "2024-06-01T10:00:00Z", ALBUM);
        entry.rows.clear();
        entry.history.clear();
        if n % 10 == 0 {
            let stack_id = Uuid::from_u128(0x0192_f1a0_0000_7000_9000_0000_0000_0000 + n);
            entry.stack = Some((stack_id, StackRole::Primary));
            stacks.push(StackEntry {
                stack_id,
                members: vec![
                    (asset_id(n), StackRole::Primary),
                    (asset_id(n + 1), StackRole::Member),
                ],
                labels: Ok(Vec::new()),
            });
        } else if n % 10 == 1 {
            let stack_id = Uuid::from_u128(0x0192_f1a0_0000_7000_9000_0000_0000_0000 + n - 1);
            entry.stack = Some((stack_id, StackRole::Member));
        }
        assets.push(entry);
    }
    Catalogue::new(assets, albums(), stacks)
}

#[test]
fn visible_window_keeps_the_selection_in_a_viewport_sized_range() {
    assert_eq!(visible_window(0, 0, 10), 0..0);
    assert_eq!(visible_window(3, 5, 0), 0..0);
    assert_eq!(visible_window(0, 5, 10), 0..5);
    assert_eq!(visible_window(4, 100, 10), 0..10);
    assert_eq!(visible_window(9, 100, 10), 0..10);
    assert_eq!(visible_window(10, 100, 10), 1..11);
    assert_eq!(visible_window(99, 100, 10), 90..100);
    // A cursor past the end is clamped to the last row.
    assert_eq!(visible_window(500, 100, 10), 90..100);
}

#[test]
fn a_frame_builds_list_items_for_the_viewport_only() {
    let catalogue = large_catalogue();
    let mut screen = LibraryScreen::default();
    screen.update(LibraryAction::Opened(Ok(catalogue.clone())));
    let timeline = catalogue.timeline().len();
    assert!(timeline > 40_000, "{timeline}");
    for step in 0..3 * 20 {
        screen.update(LibraryAction::SelectNext);
        let (items, selected) = list_window(&screen, &catalogue, 20);
        assert_eq!(items.len(), 20, "step {step}");
        assert_eq!(selected, Some((step + 1).min(19)), "step {step}");
    }
    // At the far end of a long list, still one viewport's worth.
    for _ in 0..timeline {
        screen.update(LibraryAction::SelectNext);
    }
    assert_eq!(screen.cursor(), timeline - 1);
    let (items, selected) = list_window(&screen, &catalogue, 20);
    assert_eq!((items.len(), selected), (20, Some(19)));
    // A viewport taller than the list builds the list.
    screen.update(LibraryAction::NextView);
    screen.update(LibraryAction::Enter);
    let (items, _) = list_window(&screen, &catalogue, 20);
    assert!(items.len() <= 20);
}

#[test]
fn a_large_library_draws_and_moves_with_the_selection_in_view() {
    let mut state = on_library();
    library_action(&mut state, LibraryAction::Opened(Ok(large_catalogue())));
    for _ in 0..30 {
        library_action(&mut state, LibraryAction::SelectNext);
    }
    let selected = state.library().selected_asset().expect("an asset");
    let frame = draw(&state);
    assert!(frame.contains(&selected.to_string()), "{frame}");
    assert!(frame.contains("› "), "{frame}");
}

// ── The preview decode ────────────────────────────────────────────────────

#[test]
fn only_a_decodable_still_is_read_for_a_preview() {
    assert_eq!(previewable("jpg"), Ok(()));
    assert_eq!(previewable("png"), Ok(()));
    assert_eq!(
        previewable("heic"),
        Err("this build has no decoder for image/heic stills".to_owned())
    );
    assert_eq!(
        previewable("mov"),
        Err("a .mov original is not a still this build decodes".to_owned())
    );
}

struct Panicking;

impl Decoder for Panicking {
    fn probe(&self, _: &[u8], _: &str) -> Result<MediaMetadata, MediaError> {
        panic!("a hostile original")
    }

    fn decode(&self, _: &[u8], _: &str) -> Result<DecodedImage, MediaError> {
        panic!("a hostile original")
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("encode a PNG");
    bytes.into_inner()
}

#[test]
fn a_preview_is_decoded_and_downscaled_to_the_edge() {
    let preview =
        decode_preview(&RawshiftDecoder, &png(400, 200), "png", 100).expect("a PNG decodes");
    assert_eq!((preview.width, preview.height), (100, 50));
    assert_eq!(preview.rgba.len(), 100 * 50 * 4);
    // Smaller than the edge: kept as it is.
    let preview = decode_preview(&RawshiftDecoder, &png(40, 20), "png", 100).expect("decodes");
    assert_eq!((preview.width, preview.height), (40, 20));
}

#[test]
fn a_panicking_codec_costs_the_preview_only() {
    let reason = decode_preview(&Panicking, b"bytes", "jpg", 100).expect_err("no preview");
    assert_eq!(reason, MediaError::DecoderPanic.to_string());
}

#[test]
fn bytes_that_are_no_image_say_why() {
    let reason =
        decode_preview(&RawshiftDecoder, b"not an image", "jpg", 100).expect_err("no preview");
    assert!(!reason.is_empty());
}

#[test]
fn preview_debug_leaves_out_the_pixels() {
    assert_eq!(
        format!("{:?}", image(2, 1)),
        "PreviewImage { width: 2, height: 1, .. }"
    );
}

// ── Frames ────────────────────────────────────────────────────────────────

fn draw_with(state: &AppState, canvas: &mut PreviewCanvas) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).expect("test terminal");
    terminal
        .draw(|frame| view::render_with(frame, state, canvas))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn draw(state: &AppState) -> String {
    draw_with(state, &mut PreviewCanvas::text_only())
}

fn on_library() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    state.update(Action::Open(Screen::Library));
    state
}

fn browsing() -> AppState {
    let mut state = on_library();
    state.update(Action::Library(LibraryAction::Edit));
    for c in "/photos/lib".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Next));
    for c in "pw".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Submit));
    state.update(Action::Library(LibraryAction::Opened(Ok(catalogue()))));
    state
}

fn library_action(state: &mut AppState, action: LibraryAction) {
    state.update(Action::Library(action));
}

#[test]
fn library_before_opening() {
    insta::assert_snapshot!(draw(&on_library()));
}

#[test]
fn library_that_did_not_open() {
    let mut state = on_library();
    library_action(
        &mut state,
        LibraryAction::Opened(Err(
            "Failed to open signed workspace: wrong passphrase".to_owned()
        )),
    );
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn library_timeline_loading_the_preview() {
    insta::assert_snapshot!(draw(&browsing()));
}

#[test]
fn library_timeline_on_a_terminal_without_images() {
    let mut state = browsing();
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Err(NO_IMAGE_PROTOCOL.to_owned()),
        },
    );
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn library_preview_decoded_but_drawn_as_text() {
    let mut state = browsing();
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Ok(image(640, 480)),
        },
    );
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn library_albums() {
    let mut state = browsing();
    library_action(&mut state, LibraryAction::NextView);
    library_action(&mut state, LibraryAction::SelectNext);
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn library_inside_a_stack() {
    let mut state = browsing();
    library_action(&mut state, LibraryAction::NextView);
    library_action(&mut state, LibraryAction::NextView);
    library_action(&mut state, LibraryAction::Enter);
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn library_with_no_assets() {
    let mut state = on_library();
    library_action(
        &mut state,
        LibraryAction::Opened(Ok(Catalogue::new(Vec::new(), albums(), Vec::new()))),
    );
    insta::assert_snapshot!(draw(&state));
}

/// A delegated asset shows the origin ledger's line in the detail pane, and is marked in its
/// row, while its custody is unconfirmed; once the ledger releases it both are gone.
#[test]
fn a_delegated_asset_shows_its_ledger_line_until_it_is_released() {
    const DELEGATE: Uuid = Uuid::from_u128(0x0192_f1a0_0000_7000_8000_0000_0000_00d2);
    let handed = "handed to device 000000d2, awaiting server";
    let ledger = |state: &str| -> capsule_sdk::peering::OriginLedger {
        serde_json::from_value(serde_json::json!({
            "delegations": {
                asset_id(1).to_string(): {
                    "asset_id": asset_id(1),
                    "delegate": DELEGATE,
                    "hand_off": "b3-handoff",
                    "own": [],
                    "release": [],
                    "clock_epoch": "boot",
                    "deadline_ms": 60_000,
                    "state": state,
                    "requeued_because": null,
                    "held_deletes": [],
                }
            }
        }))
        .expect("a ledger")
    };
    let open_with = |state: &str| {
        let mut app = browsing();
        let lines = crate::peer::delegation_lines(&ledger(state), crate::peer::short_delegate_name);
        let catalogue = catalogue().with_delegations(lines);
        library_action(&mut app, LibraryAction::Opened(Ok(catalogue)));
        draw(&app)
    };

    for state in ["handed_off", "acknowledged"] {
        let frame = open_with(state);
        assert!(
            frame.contains(&format!("Upload:          {handed}")),
            "the detail pane carries the ledger's line:\n{frame}"
        );
        assert!(
            frame.contains("2024-06-01 10:00  jpg  delegated"),
            "the row marks the asset:\n{frame}"
        );
    }
    let frame = open_with("released");
    assert!(
        !frame.contains("handed to") && !frame.contains("delegated"),
        "a released asset shows nothing:\n{frame}"
    );
}

#[test]
fn a_canvas_with_a_protocol_draws_the_image_instead_of_text() {
    let mut state = browsing();
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Ok(image(64, 64)),
        },
    );
    let mut canvas = PreviewCanvas::new(Some(Picker::halfblocks()));
    assert!(canvas.draws_images());
    assert!(format!("{canvas:?}").contains("Halfblocks"));

    // The frame does not encode: it says the preview is coming and asks for the encode once.
    let frame = draw_with(&state, &mut canvas);
    assert!(frame.contains("Preparing the preview"), "{frame}");
    let mut requests = canvas.take_requests();
    assert_eq!(requests.len(), 1);
    draw_with(&state, &mut canvas);
    assert!(
        canvas.take_requests().is_empty(),
        "asked again while running"
    );

    // The encode's answer reaches the state and the next frame draws it.
    let Some(Effect::Library(LibraryEffect::Encode(request))) = requests.pop() else {
        panic!("an encode request");
    };
    assert_eq!(request.image, image(64, 64));
    let (generation, size) = (request.generation, request.size);
    library_action(
        &mut state,
        LibraryAction::EncodeRequested { generation, size },
    );
    let result = encode_preview(&Picker::halfblocks(), request);
    assert!(result.is_ok(), "{result:?}");
    library_action(
        &mut state,
        LibraryAction::PreviewEncoded {
            generation,
            size,
            result,
        },
    );
    let frame = draw_with(&state, &mut canvas);
    assert!(!frame.contains("No preview"), "{frame}");
    assert!(!frame.contains("Preparing"), "{frame}");
    assert!(frame.contains('▀') || frame.contains('▄'), "{frame}");
    assert!(canvas.take_requests().is_empty(), "the encoding is reused");
}

#[test]
fn a_preview_of_the_wrong_length_does_not_encode() {
    let request = EncodeRequest {
        generation: 1,
        image: PreviewImage {
            width: 8,
            height: 8,
            rgba: vec![0; 3].into(),
        },
        size: Size::new(10, 10),
    };
    assert_eq!(
        encode_preview(&Picker::halfblocks(), request).map(|_| ()),
        Err("the decoded preview has the wrong length".to_owned())
    );
}

#[test]
fn a_panicking_encoder_costs_the_preview_only() {
    let reason = encode_guarded(|| panic!("a hostile quantizer")).expect_err("no encoding");
    assert_eq!(reason, "the terminal image encoder panicked");
    let reason = encode_guarded(|| Err("no room".to_owned())).expect_err("no encoding");
    assert_eq!(reason, "no room");
}

/// `browsing()` with the first preview decoded, and a canvas that has asked for its encode.
fn awaiting_encode() -> (AppState, PreviewCanvas, EncodeRequest) {
    let mut state = browsing();
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Ok(image(64, 64)),
        },
    );
    let mut canvas = PreviewCanvas::new(Some(Picker::halfblocks()));
    draw_with(&state, &mut canvas);
    let Some(Effect::Library(LibraryEffect::Encode(request))) = canvas.take_requests().pop() else {
        panic!("an encode request");
    };
    (state, canvas, request)
}

#[test]
fn a_failed_encode_falls_back_to_the_text_pane() {
    let (mut state, mut canvas, request) = awaiting_encode();
    library_action(
        &mut state,
        LibraryAction::EncodeRequested {
            generation: request.generation,
            size: request.size,
        },
    );
    library_action(
        &mut state,
        LibraryEffect::Encode(request).failed("the task panicked; see the TUI log".to_owned()),
    );
    let frame = draw_with(&state, &mut canvas);
    assert!(
        frame.contains("No preview: the task panicked; see the TUI log."),
        "{frame}"
    );
    // A failure is not retried frame after frame.
    assert!(canvas.take_requests().is_empty());
}

#[test]
fn an_encoding_of_a_preview_since_replaced_is_dropped() {
    let (mut state, mut canvas, request) = awaiting_encode();
    // A new decode of the same asset replaces the image before the encode answers.
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Ok(image(32, 32)),
        },
    );
    let (generation, size) = (request.generation, request.size);
    library_action(
        &mut state,
        LibraryAction::EncodeRequested { generation, size },
    );
    let result = encode_preview(&Picker::halfblocks(), request);
    library_action(
        &mut state,
        LibraryAction::PreviewEncoded {
            generation,
            size,
            result,
        },
    );
    assert_eq!(state.library().encoded, None);
    let frame = draw_with(&state, &mut canvas);
    assert!(frame.contains("Preparing the preview"), "{frame}");
    let requests = canvas.take_requests();
    assert!(
        matches!(&requests[..], [Effect::Library(LibraryEffect::Encode(r))] if r.image == image(32, 32)),
        "{requests:?}"
    );
}

/// An encode of the current preview at `size`, asked for and noted in `state`.
fn ask_encode(state: &mut AppState, size: Size) -> EncodeRequest {
    let request = EncodeRequest {
        generation: state.library().generation,
        image: image(64, 64),
        size,
    };
    library_action(
        state,
        LibraryAction::EncodeRequested {
            generation: request.generation,
            size,
        },
    );
    request
}

#[test]
fn an_older_size_encode_finishing_last_does_not_replace_the_newer() {
    let mut state = browsing();
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Ok(image(64, 64)),
        },
    );
    let (a, b) = (Size::new(20, 10), Size::new(30, 15));
    let for_a = ask_encode(&mut state, a);
    let for_b = ask_encode(&mut state, b);
    let generation = for_a.generation;
    let answer = |request: EncodeRequest| LibraryAction::PreviewEncoded {
        generation,
        size: request.size,
        result: encode_preview(&Picker::halfblocks(), request),
    };
    // B lands first, then the older A.
    library_action(&mut state, answer(for_b));
    library_action(&mut state, answer(for_a));
    let held = state.library().encoded.as_ref().expect("an encoding held");
    assert_eq!((held.0, held.1), (generation, b));
}

#[test]
fn an_older_size_encode_alone_is_dropped_and_the_current_size_is_asked_for() {
    let (mut state, mut canvas, request) = awaiting_encode();
    let asked = request.size;
    // The pane is resized: the frame asks for the new size; the old answer lands after.
    let resized = Size::new(asked.width + 7, asked.height + 3);
    let again = ask_encode(&mut state, resized);
    let generation = request.generation;
    library_action(
        &mut state,
        LibraryAction::PreviewEncoded {
            generation,
            size: asked,
            result: encode_preview(&Picker::halfblocks(), request),
        },
    );
    assert_eq!(state.library().encoded, None);
    // Still waiting on the current size, which is a request the canvas has made once.
    assert_eq!(again.size, resized);
    assert!(canvas.take_requests().is_empty());
}

#[test]
fn an_encode_for_an_asset_since_left_is_dropped() {
    let mut state = browsing();
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: asset_id(1),
            result: Ok(image(64, 64)),
        },
    );
    let size = Size::new(20, 10);
    let for_x = ask_encode(&mut state, size);
    // The selection moves to Y, whose decode lands before X's encode does.
    let effects = state.update(Action::Library(LibraryAction::SelectNext));
    let [Effect::Library(LibraryEffect::Preview(y))] = &effects[..] else {
        panic!("a preview request for Y: {effects:?}");
    };
    library_action(
        &mut state,
        LibraryAction::PreviewLoaded {
            asset_id: y.asset_id,
            result: Ok(image(32, 32)),
        },
    );
    assert_ne!(state.library().generation, for_x.generation);
    library_action(
        &mut state,
        LibraryAction::PreviewEncoded {
            generation: for_x.generation,
            size,
            result: encode_preview(&Picker::halfblocks(), for_x),
        },
    );
    assert_eq!(state.library().encoded, None);
}

// ── Through the keys and the ports ────────────────────────────────────────

/// Opens to `catalogue()` and answers every preview with a small image, counting both.
#[derive(Default)]
struct Browsing {
    opens: Arc<AtomicUsize>,
    previews: Arc<AtomicUsize>,
}

impl Ports for Browsing {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    async fn open_library(&self, request: OpenRequest) -> Result<Catalogue, String> {
        assert_eq!(
            (request.library, request.passphrase.expose()),
            (PathBuf::from("/photos/lib"), "pw")
        );
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(catalogue())
    }

    async fn preview(&self, request: PreviewRequest) -> Result<PreviewImage, String> {
        assert_eq!(request.ext, "jpg");
        self.previews.fetch_add(1, Ordering::SeqCst);
        Ok(image(8, 8))
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[tokio::test]
async fn home_leads_to_the_browser_and_the_keys_drive_it() {
    let ports = Browsing::default();
    let (opens, previews) = (Arc::clone(&ports.opens), Arc::clone(&ports.previews));
    let mut d = Driver::new(ports, 100, 26);
    d.settle().await;
    assert!(d.frame().contains("b Browse a library"), "{}", d.frame());

    d.press(key(KeyCode::Char('b')));
    assert_eq!(d.state().screen(), Screen::Library);
    d.press(key(KeyCode::Char('o')));
    d.type_keys("/photos/lib");
    d.press(key(KeyCode::Tab));
    d.type_keys("pw");
    d.press(key(KeyCode::Enter));
    assert!(d.frame().contains("Opening the library…"), "{}", d.frame());
    d.settle().await;
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(previews.load(Ordering::SeqCst), 1);
    assert!(d.frame().contains("Timeline"), "{}", d.frame());
    assert!(d.frame().contains("History"), "{}", d.frame());

    d.press(key(KeyCode::Down));
    d.settle().await;
    assert_eq!(previews.load(Ordering::SeqCst), 2);
    d.press(key(KeyCode::Char('v')));
    d.press(key(KeyCode::Enter));
    assert_eq!(d.state().library().group(), Some(Group::Album(ALBUM)));
    d.press(key(KeyCode::Left));
    assert_eq!(d.state().library().group(), None);
    d.press(key(KeyCode::Esc));
    assert_eq!(d.state().screen(), Screen::Home);
}

/// A terminal that draws images, whose encoder panics on every preview.
#[derive(Default)]
struct PanickingEncoder {
    encodes: Arc<AtomicUsize>,
}

impl Ports for PanickingEncoder {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    async fn open_library(&self, _: OpenRequest) -> Result<Catalogue, String> {
        Ok(catalogue())
    }

    async fn preview(&self, _: PreviewRequest) -> Result<PreviewImage, String> {
        Ok(image(16, 16))
    }

    async fn encode_preview(&self, _: EncodeRequest) -> Result<EncodedPreview, String> {
        self.encodes.fetch_add(1, Ordering::SeqCst);
        panic!("the encoder blew up")
    }

    fn image_picker(&self) -> Option<Picker> {
        Some(Picker::halfblocks())
    }
}

/// The real loop asks for the encode after the frame, off the loop; the encoder panics on its
/// task, the pane falls back to text, and the loop keeps going.
#[tokio::test(start_paused = true)]
async fn a_panicking_encoder_behind_the_port_leaves_the_text_pane() {
    use std::time::Duration;

    use futures::StreamExt as _;

    let ports = PanickingEncoder::default();
    let encodes = Arc::clone(&ports.encodes);
    let mut keys: Vec<std::io::Result<crossterm::event::Event>> = Vec::new();
    for code in [KeyCode::Char('b'), KeyCode::Char('o')] {
        keys.push(Ok(crossterm::event::Event::Key(key(code))));
    }
    for c in "/photos/lib".chars() {
        keys.push(Ok(crossterm::event::Event::Key(key(KeyCode::Char(c)))));
    }
    keys.push(Ok(crossterm::event::Event::Key(key(KeyCode::Tab))));
    for c in "pw".chars() {
        keys.push(Ok(crossterm::event::Event::Key(key(KeyCode::Char(c)))));
    }
    keys.push(Ok(crossterm::event::Event::Key(key(KeyCode::Enter))));
    // A quiet second for every effect to answer, then a resize (a redraw, no action), then the
    // input ends.
    let events = futures::stream::iter(keys).chain(futures::stream::once(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        Ok(crossterm::event::Event::Resize(100, 26))
    }));
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).expect("test terminal");
    let state = crate::tui::runtime::event_loop(&mut terminal, Box::pin(events), Arc::new(ports))
        .await
        .expect("the loop survives");

    assert_eq!(
        encodes.load(Ordering::SeqCst),
        1,
        "asked once, not per frame"
    );
    assert!(
        matches!(&state.library().encoded, Some((_, _, Err(reason))) if reason.contains("panicked")),
        "{:?}",
        state.library().encoded
    );
    let frame = frame_text(terminal.backend().buffer());
    assert!(frame.contains("No preview: the task panicked"), "{frame}");
}
