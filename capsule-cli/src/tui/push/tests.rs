//! Push screen tests: the form, the request it sends, the fold of the run's progress into
//! per-asset custody rows, the retry of what did not reach custody, and the frames it draws.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::remote::AssetPushFailure;
use crate::tui::driver::frame_text;
use crate::tui::import::ImportAction;
use crate::tui::state::{Action, AppState, Screen, SessionSummary};
use crate::tui::view;

const A: &str = "0192f1a0-0000-7000-8000-00000000000a";
const B: &str = "0192f1a0-0000-7000-8000-00000000000b";
const C: &str = "0192f1a0-0000-7000-8000-00000000000c";

/// Fill the form and submit it.
fn submit(screen: &mut PushScreen, library: &str, passphrase: &str) -> Vec<Effect> {
    screen.form.set(LIBRARY, library);
    screen.form.set(PASSPHRASE, passphrase);
    screen.on_form_key(FormKey::Submit)
}

/// A screen with a whole-library run in flight.
fn running() -> PushScreen {
    let mut screen = PushScreen::default();
    screen.update(PushAction::Edit);
    submit(&mut screen, "/photos/lib", "pw");
    screen
}

fn blob(asset: &str, n: usize) -> String {
    format!("{}{n:02}", &asset[asset.len() - 2..])
}

fn started(asset: &str, blobs: usize) -> PushProgress {
    PushProgress::Started {
        asset_id: asset.to_owned(),
        blobs: (0..blobs).map(|n| blob(asset, n)).collect(),
    }
}

fn finalized(asset: &str, n: usize) -> PushProgress {
    PushProgress::Transfer(TransferEvent::Finalize {
        blob: blob(asset, n),
        session_id: "s".to_owned(),
        size: 10,
    })
}

fn merged(asset: &str, n: usize) -> PushProgress {
    PushProgress::Transfer(TransferEvent::Merged {
        blob: blob(asset, n),
        asset_ref: "other".to_owned(),
    })
}

fn verified(asset: &str, durable: bool) -> PushProgress {
    PushProgress::Transfer(TransferEvent::Verify {
        asset_id: asset.to_owned(),
        durable,
        blobs: 3,
        not_durable: u64::from(!durable),
    })
}

fn receipt(asset: &str, n: usize) -> PushProgress {
    PushProgress::Transfer(TransferEvent::Receipt {
        asset_id: asset.to_owned(),
        blob: blob(asset, n),
        receipt_seq: n as u64,
    })
}

fn outcome(asset: &str, failure: Option<(&'static str, &str)>) -> AssetPushOutcome {
    AssetPushOutcome {
        asset_id: asset.to_owned(),
        status: if failure.is_some() {
            AssetPushStatus::Failed
        } else {
            AssetPushStatus::Verified
        },
        uploaded: if failure.is_some() { 1 } else { 2 },
        merged: 0,
        already_held: usize::from(failure.is_none()),
        deferred: 0,
        bytes: 30,
        receipts: if failure.is_some() { 0 } else { 3 },
        resealed: false,
        error: failure.map(|(kind, message)| AssetPushFailure {
            kind,
            code: None,
            message: message.to_owned(),
        }),
    }
}

fn summary(outcomes: Vec<AssetPushOutcome>) -> PushSummary {
    let failed = outcomes
        .iter()
        .filter(|o| o.status == AssetPushStatus::Failed)
        .count();
    PushSummary {
        assets: outcomes.len(),
        verified_assets: outcomes.len() - failed,
        failed_assets: failed,
        receipts: outcomes.iter().map(|o| o.receipts).sum(),
        outcomes,
        ..PushSummary::default()
    }
}

fn watch() -> Vec<Effect> {
    vec![Effect::Push(PushEffect::Watch)]
}

#[test]
fn a_whole_library_push_runs_and_watches_and_forgets_the_passphrase() {
    let mut screen = PushScreen::default();
    screen.update(PushAction::Edit);
    assert!(screen.is_editing());
    let effects = submit(&mut screen, "/photos/lib", "pass phrase");
    assert_eq!(
        effects,
        vec![
            Effect::Push(PushEffect::Watch),
            Effect::Push(PushEffect::Run(PushRequest {
                run: 1,
                library: PathBuf::from("/photos/lib"),
                passphrase: Secret::new("pass phrase"),
                assets: None,
            })),
        ]
    );
    assert_eq!(screen.run(), Some(&Loadable::Loading));
    assert!(screen.is_watching());
    assert!(!screen.is_editing());
    assert!(screen.form().secret(PASSPHRASE).is_empty());
    assert_eq!(screen.form().value(LIBRARY), "/photos/lib");
}

#[test]
fn the_form_is_refused_before_any_request() {
    for (library, passphrase, reason) in [
        ("", "pw", "Enter the library."),
        ("/lib", "", "Enter the library passphrase."),
    ] {
        let mut screen = PushScreen::default();
        screen.update(PushAction::Edit);
        assert!(submit(&mut screen, library, passphrase).is_empty());
        assert_eq!(screen.notice(), Some(reason));
        assert!(screen.is_editing());
        assert_eq!(screen.run(), None);
    }
}

#[test]
fn a_second_push_waits_for_the_first() {
    let mut screen = running();
    screen.update(PushAction::Edit);
    assert!(!screen.is_editing());
    screen.update(PushAction::Retry);
    assert!(!screen.is_editing());
}

#[test]
fn custody_steps_fill_the_row_of_the_asset_they_belong_to() {
    let mut screen = running();
    let effects = screen.update(PushAction::Progress(vec![started(A, 3)]));
    assert_eq!(effects, watch(), "the watch is asked for again");
    let row = screen.row(A).expect("a row");
    assert_eq!(row.custody, Custody::Sending);
    assert_eq!((row.blobs, row.uploaded, row.receipts), (3, 0, 0));

    screen.update(PushAction::Progress(vec![
        finalized(A, 0),
        merged(A, 1),
        // A blob nobody started is not this asset's: dropped.
        finalized(B, 0),
    ]));
    assert_eq!(screen.row(A).map(|r| r.uploaded), Some(2));
    assert_eq!(screen.row(B), None);

    screen.update(PushAction::Progress(vec![
        verified(A, true),
        receipt(A, 0),
        receipt(A, 1),
    ]));
    let row = screen.row(A).expect("a row");
    assert_eq!(row.durable, Some(true));
    assert_eq!(row.receipts, 2);
    assert_eq!(
        row.custody,
        Custody::Sending,
        "custody is the outcome's to say"
    );

    screen.update(PushAction::Progress(vec![PushProgress::Finished(outcome(
        A, None,
    ))]));
    let row = screen.row(A).expect("a row");
    assert_eq!(row.custody, Custody::Verified);
    assert_eq!((row.uploaded, row.blobs, row.receipts), (3, 3, 3));
}

#[test]
fn a_step_after_its_asset_finished_changes_nothing() {
    let mut screen = running();
    screen.update(PushAction::Progress(vec![
        started(A, 3),
        PushProgress::Finished(outcome(A, Some(("not_durable", "not durable")))),
        receipt(A, 0),
        finalized(A, 2),
    ]));
    let row = screen.row(A).expect("a row");
    assert_eq!(row.receipts, 0);
    assert_eq!(row.uploaded, 1, "as the outcome says");
    assert_eq!(row.durable, Some(false), "a not-durable failure says so");
    assert_eq!(row.custody, Custody::Failed("not durable".to_owned()));
}

#[test]
fn the_watch_stops_only_when_this_runs_stream_closes() {
    let mut screen = running();
    let effects = screen.update(PushAction::Progress(vec![PushProgress::Closed {
        run: Some(0),
    }]));
    assert_eq!(effects, watch(), "a close left over from another run");
    assert!(screen.is_watching());
    let effects = screen.update(PushAction::Progress(vec![
        started(A, 1),
        PushProgress::Closed { run: Some(1) },
    ]));
    assert!(effects.is_empty());
    assert!(!screen.is_watching());
    assert_eq!(
        screen.row(A).map(|r| &r.custody),
        Some(&Custody::Sending),
        "what came before the close in the batch is folded"
    );
}

#[test]
fn a_failed_watch_stops_watching() {
    let mut screen = running();
    let action = PushEffect::Watch.failed("the task panicked; see the TUI log".to_owned());
    assert_eq!(
        action,
        PushAction::Progress(vec![PushProgress::Closed { run: None }])
    );
    assert!(screen.update(action).is_empty());
    assert!(!screen.is_watching());
}

#[test]
fn lagged_events_are_counted() {
    let mut screen = running();
    screen.update(PushAction::Progress(vec![PushProgress::Lagged(3)]));
    screen.update(PushAction::Progress(vec![PushProgress::Lagged(4)]));
    assert_eq!(screen.lagged(), 7);
}

#[test]
fn the_summary_is_the_truth_and_progress_after_it_is_dropped() {
    let mut screen = running();
    screen.update(PushAction::Progress(vec![started(A, 3), started(B, 2)]));
    screen.update(PushAction::Finished(Ok(summary(vec![
        outcome(A, None),
        outcome(B, Some(("upload", "the server refused the chunk"))),
        outcome(C, Some(("bundle", "asset not found"))),
    ]))));
    assert_eq!(screen.row(A).map(|r| &r.custody), Some(&Custody::Verified));
    assert_eq!(
        screen.row(C).map(|r| &r.custody),
        Some(&Custody::Failed("asset not found".to_owned())),
        "an asset the progress never mentioned still gets its row"
    );
    // The summary overtook the watch: what the watch brings now is stale.
    screen.update(PushAction::Progress(vec![started(A, 3), receipt(B, 0)]));
    assert_eq!(screen.row(A).map(|r| &r.custody), Some(&Custody::Verified));
    assert_eq!(screen.row(B).map(|r| r.receipts), Some(0));
}

#[test]
fn a_run_that_stopped_leaves_its_unfinished_assets_not_reached_and_retryable() {
    let mut screen = running();
    screen.update(PushAction::Progress(vec![
        started(A, 1),
        PushProgress::Finished(outcome(A, None)),
        started(B, 2),
    ]));
    screen.update(PushAction::Finished(Err(
        "Not authenticated. Run 'capsule auth login' first.".to_owned(),
    )));
    assert_eq!(screen.row(B).map(|r| &r.custody), Some(&Custody::Waiting));
    let retryable: Vec<&str> = screen
        .retryable()
        .iter()
        .map(|r| r.asset_id.as_str())
        .collect();
    assert_eq!(retryable, vec![B]);
}

#[test]
fn a_retry_pushes_only_what_did_not_reach_custody_and_keeps_the_rest() {
    let mut screen = running();
    screen.update(PushAction::Progress(vec![
        started(A, 3),
        started(B, 2),
        started(C, 2),
    ]));
    screen.update(PushAction::Finished(Ok(summary(vec![
        outcome(A, None),
        outcome(B, Some(("upload", "connection reset"))),
        outcome(C, Some(("receipt_missing", "no receipt for a blob"))),
    ]))));
    screen.update(PushAction::Progress(vec![PushProgress::Closed {
        run: Some(1),
    }]));

    screen.update(PushAction::Retry);
    assert!(screen.is_editing());
    assert_eq!(screen.form().focus(), PASSPHRASE, "the library is known");
    let effects = screen.on_form_key(FormKey::Char('p'));
    assert!(effects.is_empty());
    let effects = screen.on_form_key(FormKey::Submit);
    assert_eq!(
        effects,
        vec![
            Effect::Push(PushEffect::Watch),
            Effect::Push(PushEffect::Run(PushRequest {
                run: 2,
                library: PathBuf::from("/photos/lib"),
                passphrase: Secret::new("p"),
                assets: Some(vec![B.parse().unwrap(), C.parse().unwrap()]),
            })),
        ]
    );
    assert_eq!(screen.row(A).map(|r| &r.custody), Some(&Custody::Verified));
    let b = screen.row(B).expect("a row");
    assert_eq!(b.custody, Custody::Waiting, "reset for the new attempt");
    assert_eq!((b.uploaded, b.receipts, b.durable), (0, 0, None));
}

#[test]
fn a_retry_while_the_last_watch_is_still_out_does_not_start_another() {
    let mut screen = running();
    screen.update(PushAction::Finished(Ok(summary(vec![outcome(
        A,
        Some(("upload", "reset")),
    )]))));
    assert!(screen.is_watching(), "the close has not arrived yet");
    screen.update(PushAction::Retry);
    let effects = submit(&mut screen, "/photos/lib", "pw");
    assert_eq!(
        effects.len(),
        1,
        "the watch already out carries on: {effects:?}"
    );
    // That watch now brings the old run's close, which does not end the new one.
    let effects = screen.update(PushAction::Progress(vec![PushProgress::Closed {
        run: Some(1),
    }]));
    assert_eq!(effects, watch());
}

#[test]
fn a_cancelled_retry_forgets_which_assets_it_was_for() {
    let mut screen = running();
    screen.update(PushAction::Finished(Ok(summary(vec![outcome(
        A,
        Some(("upload", "reset")),
    )]))));
    screen.update(PushAction::Retry);
    screen.on_form_key(FormKey::Cancel);
    screen.update(PushAction::Edit);
    let effects = submit(&mut screen, "/photos/lib", "pw");
    assert!(matches!(
        effects.last(),
        Some(Effect::Push(PushEffect::Run(PushRequest {
            assets: None,
            ..
        })))
    ));
    assert!(screen.rows().is_empty(), "a whole-library run starts over");
}

#[test]
fn nothing_to_retry_says_so() {
    let mut screen = PushScreen::default();
    screen.update(PushAction::Retry);
    assert!(!screen.is_editing());
    assert_eq!(
        screen.notice(),
        Some("Nothing to retry yet: press e to push a library.")
    );
    let mut screen = running();
    screen.update(PushAction::Finished(Ok(summary(vec![outcome(A, None)]))));
    screen.update(PushAction::Retry);
    assert!(!screen.is_editing());
    assert_eq!(
        screen.notice(),
        Some("Nothing to retry: every asset reached custody.")
    );
}

#[test]
fn a_failed_run_effect_reports_why() {
    let request = PushRequest {
        run: 1,
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
        assets: None,
    };
    let action = PushEffect::Run(request).failed("the task panicked".to_owned());
    assert_eq!(
        action,
        PushAction::Finished(Err("the task panicked".to_owned()))
    );
}

#[test]
fn only_custody_steps_are_relayed() {
    let custody = [
        finalized(A, 0),
        merged(A, 0),
        verified(A, true),
        receipt(A, 0),
    ];
    for progress in custody {
        let PushProgress::Transfer(event) = progress else {
            unreachable!()
        };
        assert!(is_custody_step(&event), "{event:?}");
    }
    let chunk = TransferEvent::ChunkSent {
        blob: "b".to_owned(),
        session_id: "s".to_owned(),
        offset: 0,
        len: 1,
    };
    assert!(!is_custody_step(&chunk));
}

#[test]
fn the_passphrase_never_reaches_debug() {
    let mut screen = PushScreen::default();
    screen.update(PushAction::Edit);
    screen.form.set(LIBRARY, "/lib");
    screen.form.set(PASSPHRASE, "hunter2 secret");
    assert!(!format!("{screen:?}").contains("hunter2"));
    let effects = screen.on_form_key(FormKey::Submit);
    assert!(!format!("{effects:?}").contains("hunter2"));
}

#[test]
fn asset_progress_converts_with_the_id_as_text() {
    let id: Uuid = A.parse().unwrap();
    assert_eq!(
        PushProgress::from(AssetProgress::Started {
            asset_id: id,
            blobs: vec!["ab".to_owned()],
        }),
        PushProgress::Started {
            asset_id: A.to_owned(),
            blobs: vec!["ab".to_owned()],
        }
    );
    let finished = outcome(A, None);
    assert_eq!(
        PushProgress::from(AssetProgress::Finished(finished.clone())),
        PushProgress::Finished(finished)
    );
}

#[test]
fn a_library_is_offered_only_to_an_empty_field() {
    let mut screen = PushScreen::default();
    screen.offer_library("/photos/lib");
    assert_eq!(screen.form().value(LIBRARY), "/photos/lib");
    screen.offer_library("/other");
    assert_eq!(
        screen.form().value(LIBRARY),
        "/photos/lib",
        "a typed library stays"
    );
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 30)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn on_push() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    state.update(Action::Open(Screen::Push));
    state
}

fn start(state: &mut AppState) {
    state.update(Action::Push(PushAction::Edit));
    for c in "/photos/lib".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Next));
    for c in "pw".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Submit));
}

fn progress(state: &mut AppState, batch: Vec<PushProgress>) {
    state.update(Action::Push(PushAction::Progress(batch)));
}

#[test]
fn push_before_any_run() {
    insta::assert_snapshot!(draw(&on_push()));
}

#[test]
fn push_mid_run_shows_each_assets_custody() {
    let mut state = on_push();
    start(&mut state);
    progress(
        &mut state,
        vec![
            started(A, 3),
            finalized(A, 0),
            finalized(A, 1),
            merged(A, 2),
            verified(A, true),
            receipt(A, 0),
            receipt(A, 1),
            receipt(A, 2),
            PushProgress::Finished(outcome(A, None)),
            started(B, 2),
            PushProgress::Finished(outcome(B, Some(("upload", "connection reset")))),
            started(C, 4),
            finalized(C, 0),
            PushProgress::Lagged(5),
        ],
    );
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn push_finished_with_a_failure_to_retry() {
    let mut state = on_push();
    start(&mut state);
    progress(&mut state, vec![started(A, 3), started(B, 2)]);
    state.update(Action::Push(PushAction::Finished(Ok(summary(vec![
        outcome(A, None),
        outcome(B, Some(("not_durable", "2 of 2 blobs are not durable yet"))),
    ])))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn push_retry_form() {
    let mut state = on_push();
    start(&mut state);
    state.update(Action::Push(PushAction::Finished(Ok(summary(vec![
        outcome(A, None),
        outcome(B, Some(("upload", "connection reset"))),
    ])))));
    state.update(Action::Push(PushAction::Retry));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn push_that_could_not_run() {
    let mut state = on_push();
    start(&mut state);
    state.update(Action::Push(PushAction::Finished(Err(
        "Not authenticated. Run 'capsule auth login' first.".to_owned(),
    ))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn many_assets_show_the_busy_and_failed_ones_first_and_count_the_rest() {
    let mut state = on_push();
    start(&mut state);
    let ids: Vec<String> = (0..12)
        .map(|n| format!("0192f1a0-0000-7000-8000-0000000001{n:02}"))
        .collect();
    let mut batch = Vec::new();
    for (n, id) in ids.iter().enumerate() {
        batch.push(started(id, 1));
        let failure = (n == 9).then_some(("upload", "reset"));
        batch.push(PushProgress::Finished(outcome(id, failure)));
    }
    batch.push(started(A, 2));
    progress(&mut state, batch);
    let frame = draw(&state);
    let rows: Vec<&str> = frame
        .lines()
        .filter(|line| line.contains("0192f1a0"))
        .collect();
    assert!(
        rows[0].contains(A) && rows[0].contains("sending"),
        "{frame}"
    );
    assert!(
        rows[1].contains(&ids[9]) && rows[1].contains("failed"),
        "{frame}"
    );
    assert!(frame.contains("…and 5 more."), "{frame}");
}

#[test]
fn a_verdict_that_counts_a_blob_not_durable_is_not_shown_durable() {
    let mut screen = running();
    screen.update(PushAction::Progress(vec![started(A, 3)]));
    // The SDK rejects this verdict as NotDurable whatever its flag says.
    screen.update(PushAction::Progress(vec![PushProgress::Transfer(
        TransferEvent::Verify {
            asset_id: A.to_owned(),
            durable: true,
            blobs: 3,
            not_durable: 1,
        },
    )]));
    assert_eq!(screen.rows[0].durable, Some(false));
}

#[test]
fn a_push_to_a_library_an_import_is_writing_is_refused_and_says_why() {
    let mut state = on_push();
    // An import into the same library is in flight.
    state.update(Action::Open(Screen::Import));
    state.update(Action::Import(ImportAction::AddSource));
    for c in "/a".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Submit));
    state.update(Action::Import(ImportAction::Edit));
    for c in "/photos/lib".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Next));
    for c in "pw".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Submit));
    assert!(state.import().running_library().is_some());

    state.update(Action::Open(Screen::Push));
    start(&mut state);
    assert_eq!(state.push().running_library(), None, "no push started");
    assert!(state.push().is_editing(), "the form stays open to fix");
    assert!(
        state.push().notice().is_some_and(|n| n.contains("import")),
        "{:?}",
        state.push().notice()
    );
}

#[test]
fn a_retry_to_a_library_an_import_is_writing_is_refused() {
    let mut retry = PushScreen::default();
    retry.update(PushAction::Edit);
    submit(&mut retry, "/photos/lib", "pw");
    retry.update(PushAction::Finished(Ok(summary(vec![outcome(
        A,
        Some(("upload", "boom")),
    )]))));
    retry.update(PushAction::Retry);
    retry.form.set(LIBRARY, "/photos/lib");
    retry.form.set(PASSPHRASE, "pw");
    let effects = retry.on_form_key_guarded(FormKey::Submit, Some(Path::new("/photos/lib")));
    assert!(effects.is_empty());
    assert_eq!(retry.running_library(), None);

    // Another library is not in the way.
    retry.form.set(LIBRARY, "/other");
    let effects = retry.on_form_key_guarded(FormKey::Submit, Some(Path::new("/photos/lib")));
    assert!(!effects.is_empty());
    assert_eq!(retry.running_library(), Some(Path::new("/other")));
}
