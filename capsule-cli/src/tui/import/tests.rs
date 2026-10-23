//! Import screen tests: the source picker, the form's checks, the request it sends, and the
//! frames it draws.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::import_report::FileImportOutcome;
use crate::tui::driver::frame_text;
use crate::tui::state::{Action, AppState, Screen, SessionSummary};
use crate::tui::view;

/// Type `text` into whichever form is being edited, then submit it.
fn type_and_submit(screen: &mut ImportScreen, text: &str) -> Vec<Effect> {
    for c in text.chars() {
        screen.on_form_key(FormKey::Char(c));
    }
    screen.on_form_key(FormKey::Submit)
}

/// Pick `path` through the add-source form.
fn add(screen: &mut ImportScreen, path: &str) {
    screen.update(ImportAction::AddSource);
    assert!(screen.is_editing());
    assert!(type_and_submit(screen, path).is_empty());
    assert!(!screen.is_editing());
}

/// A screen with `sources` picked.
fn picked(sources: &[&str]) -> ImportScreen {
    let mut screen = ImportScreen::default();
    for source in sources {
        add(&mut screen, source);
    }
    screen
}

/// Fill the library form with `library` and `passphrase` and submit it.
fn run(screen: &mut ImportScreen, library: &str, passphrase: &str) -> Vec<Effect> {
    screen.update(ImportAction::Edit);
    screen.form.set(LIBRARY, library);
    screen.form.set(PASSPHRASE, passphrase);
    screen.on_form_key(FormKey::Submit)
}

fn paths(sources: &[&str]) -> Vec<PathBuf> {
    sources.iter().map(PathBuf::from).collect()
}

#[test]
fn sources_are_added_in_order_and_the_newest_is_selected() {
    let screen = picked(&["/photos/2019", "/photos/2020"]);
    assert_eq!(screen.sources(), paths(&["/photos/2019", "/photos/2020"]));
    assert_eq!(screen.selected(), 1);
    assert_eq!(screen.invalid(), None);
}

#[test]
fn a_blank_or_repeated_source_is_refused_and_the_repeat_selected() {
    let mut screen = picked(&["/photos/2019", "/photos/2020"]);
    add(&mut screen, "   ");
    assert_eq!(screen.invalid(), Some("Type a file or folder path to add."));
    add(&mut screen, "/photos/2019");
    assert_eq!(screen.invalid(), Some("/photos/2019 is already picked."));
    assert_eq!(screen.selected(), 0, "the repeat is selected");
    assert_eq!(screen.sources().len(), 2);
}

#[test]
fn a_cancelled_source_adds_nothing() {
    let mut screen = ImportScreen::default();
    screen.update(ImportAction::AddSource);
    screen.on_form_key(FormKey::Char('/'));
    screen.on_form_key(FormKey::Cancel);
    assert!(!screen.is_editing());
    assert!(screen.sources().is_empty());
}

#[test]
fn selection_wraps_and_removing_keeps_it_in_range() {
    let mut screen = picked(&["/a", "/b", "/c"]);
    screen.update(ImportAction::SelectNext);
    assert_eq!(screen.selected(), 0, "past the last wraps to the first");
    screen.update(ImportAction::SelectPrevious);
    assert_eq!(screen.selected(), 2, "before the first wraps to the last");
    screen.update(ImportAction::RemoveSource);
    assert_eq!(screen.sources(), paths(&["/a", "/b"]));
    assert_eq!(
        screen.selected(),
        1,
        "the selection moves onto the new last"
    );
    screen.update(ImportAction::SelectPrevious);
    screen.update(ImportAction::RemoveSource);
    assert_eq!(screen.sources(), paths(&["/b"]));
    assert_eq!(screen.selected(), 0);
    screen.update(ImportAction::RemoveSource);
    assert!(screen.sources().is_empty());
    // Nothing left to remove or select: no panic, nothing changes.
    screen.update(ImportAction::RemoveSource);
    screen.update(ImportAction::SelectNext);
    screen.update(ImportAction::SelectPrevious);
    assert_eq!(screen.selected(), 0);
}

#[test]
fn the_kind_switches_between_files_and_a_takeout_export() {
    let mut screen = ImportScreen::default();
    assert_eq!(screen.kind(), SourceKind::Files);
    assert_eq!(screen.kind().provider(), None);
    screen.update(ImportAction::SwitchKind);
    assert_eq!(screen.kind(), SourceKind::Takeout);
    assert_eq!(screen.kind().provider(), Some(ImportProviderArg::Takeout));
    screen.update(ImportAction::SwitchKind);
    assert_eq!(screen.kind(), SourceKind::Files);
}

#[test]
fn submitting_sends_every_source_and_forgets_the_passphrase() {
    let mut screen = picked(&["/takeout/part1", "/takeout/part2"]);
    screen.update(ImportAction::SwitchKind);
    let effects = run(&mut screen, "/photos/lib", "pass phrase");
    assert_eq!(
        effects,
        vec![Effect::Import(ImportEffect::Run(ImportRequest {
            library: PathBuf::from("/photos/lib"),
            passphrase: Secret::new("pass phrase"),
            kind: SourceKind::Takeout,
            sources: paths(&["/takeout/part1", "/takeout/part2"]),
        }))]
    );
    assert_eq!(screen.run(), Some(&Loadable::Loading));
    assert!(!screen.is_editing());
    assert!(screen.form().secret(PASSPHRASE).is_empty());
    assert_eq!(
        screen.library(),
        "/photos/lib",
        "the library stays for the push"
    );
}

#[test]
fn the_form_is_refused_before_any_request() {
    let cases = [
        (
            Vec::new(),
            "/lib",
            "pw",
            "Pick at least one source first: press a to add one.",
        ),
        (vec!["/a"], "", "pw", "Enter the library."),
        (vec!["/a"], "/lib", "", "Enter the library passphrase."),
    ];
    for (sources, library, passphrase, reason) in cases {
        let mut screen = picked(&sources);
        let effects = run(&mut screen, library, passphrase);
        assert!(effects.is_empty(), "{reason}");
        assert_eq!(screen.invalid(), Some(reason));
        assert!(screen.is_editing(), "the form stays open to fix: {reason}");
        assert_eq!(screen.run(), None);
    }
}

#[test]
fn a_second_import_waits_for_the_first() {
    let mut screen = picked(&["/a"]);
    run(&mut screen, "/lib", "pw");
    screen.update(ImportAction::Edit);
    assert!(!screen.is_editing(), "the library is busy");
    // The picker still works while it runs.
    add(&mut screen, "/b");
    assert_eq!(screen.sources().len(), 2);
}

#[test]
fn a_failed_effect_reports_why() {
    let effect = ImportEffect::Run(ImportRequest {
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
        kind: SourceKind::Files,
        sources: paths(&["/a"]),
    });
    let mut screen = ImportScreen::default();
    screen.update(effect.failed("the task panicked; see the TUI log".to_owned()));
    assert_eq!(
        screen.run(),
        Some(&Loadable::Failed(
            "the task panicked; see the TUI log".to_owned()
        ))
    );
}

#[test]
fn the_passphrase_never_reaches_debug() {
    let mut screen = picked(&["/a"]);
    screen.update(ImportAction::Edit);
    screen.form.set(LIBRARY, "/lib");
    screen.form.set(PASSPHRASE, "hunter2 secret");
    assert!(!format!("{screen:?}").contains("hunter2"));
    let effects = screen.on_form_key(FormKey::Submit);
    assert!(!format!("{effects:?}").contains("hunter2"));
}

fn outcome(path: &str, status: FileImportStatus, error: Option<&str>) -> FileImportOutcome {
    FileImportOutcome {
        path: path.to_owned(),
        status,
        asset_id: None,
        derivatives: None,
        deferred_formats: 0,
        error: error.map(str::to_owned),
    }
}

/// A report of `imported` files and `failed` unreadable ones.
fn report(imported: usize, failed: usize) -> ImportReport {
    let mut outcomes = Vec::new();
    for i in 0..imported {
        outcomes.push(outcome(
            &format!("/photos/ok-{i}.jpg"),
            FileImportStatus::Imported,
            None,
        ));
    }
    for i in 0..failed {
        outcomes.push(outcome(
            &format!("/photos/bad-{i}.jpg"),
            FileImportStatus::Failed,
            Some("permission denied"),
        ));
    }
    ImportReport {
        candidates: imported + failed,
        files: imported + failed,
        planned: imported,
        imported,
        errors: failed,
        outcomes,
        ..ImportReport::default()
    }
}

#[test]
fn failures_past_the_limit_are_counted() {
    let lines = report_lines(&report(1, FAILURES_SHOWN + 2));
    let text: Vec<String> = lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(text.len(), 1 + FAILURES_SHOWN + 2, "{text:?}");
    assert_eq!(
        text[FAILURES_SHOWN + 1],
        "…and 2 more; `capsule import --json` lists every one."
    );
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn on_import() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    state.update(Action::Open(Screen::Import));
    state
}

fn type_text(state: &mut AppState, text: &str) {
    for c in text.chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
}

fn pick(state: &mut AppState, path: &str) {
    state.update(Action::Import(ImportAction::AddSource));
    type_text(state, path);
    state.update(Action::Form(FormKey::Submit));
}

#[test]
fn import_before_picking_anything() {
    insta::assert_snapshot!(draw(&on_import()));
}

#[test]
fn import_with_a_takeout_export_picked_and_the_form_open() {
    let mut state = on_import();
    state.update(Action::Import(ImportAction::SwitchKind));
    pick(&mut state, "/downloads/takeout-001");
    pick(&mut state, "/downloads/takeout-002");
    state.update(Action::Import(ImportAction::SelectPrevious));
    state.update(Action::Import(ImportAction::Edit));
    type_text(&mut state, "/photos/lib");
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn import_finished_with_failures() {
    let mut state = on_import();
    pick(&mut state, "/photos");
    state.update(Action::Import(ImportAction::Finished(Ok(report(3, 2)))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn import_that_could_not_run() {
    let mut state = on_import();
    pick(&mut state, "/photos");
    state.update(Action::Import(ImportAction::Finished(Err(
        "Failed to open signed workspace: wrong passphrase".to_owned(),
    ))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn a_finished_import_offers_its_library_to_the_push_screen() {
    let mut state = on_import();
    pick(&mut state, "/photos");
    state.update(Action::Import(ImportAction::Edit));
    type_text(&mut state, "/photos/lib");
    state.update(Action::Form(FormKey::Next));
    type_text(&mut state, "pw");
    state.update(Action::Form(FormKey::Submit));
    assert_eq!(state.push().form().value(0), "", "not before it finished");
    state.update(Action::Import(ImportAction::Finished(Ok(report(1, 0)))));
    assert_eq!(state.push().form().value(0), "/photos/lib");
    assert!(
        state.push().form().secret(1).is_empty(),
        "the passphrase is never carried over"
    );
}

#[test]
fn a_failed_import_offers_nothing() {
    let mut state = on_import();
    state.update(Action::Import(ImportAction::Edit));
    type_text(&mut state, "/photos/lib");
    state.update(Action::Form(FormKey::Cancel));
    state.update(Action::Import(ImportAction::Finished(Err("no".to_owned()))));
    assert_eq!(state.push().form().value(0), "");
}
