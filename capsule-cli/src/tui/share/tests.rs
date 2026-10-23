//! Share-links screen tests: the issue and revoke forms, the requests they send, the link they
//! show, and that the link's URL never reaches a log line.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::state::{Action, AppState, Screen, SessionSummary};
use crate::tui::view;

const ALBUM_ID: &str = "0192f1a0-0000-7000-8000-000000000001";
const ASSET_ID: &str = "0192f1a0-0000-7000-8000-000000000002";
const OPAQUE: &str = "0123456789abcdef0123456789abcdef";
const URL: &str = "http://127.0.0.1:3000/s/0123456789abcdef0123456789abcdef#fragment-secret";

fn creating(values: &[&str]) -> ShareScreen {
    let mut screen = ShareScreen::default();
    screen.update(ShareAction::BeginCreate);
    for (index, value) in values.iter().enumerate() {
        screen.create.set(index, value);
    }
    screen
}

fn link(protected: bool) -> IssuedLink {
    IssuedLink {
        opaque_id: OPAQUE.to_owned(),
        url: Secret::new(URL),
        covered: 3,
        passphrase_protected: protected,
        expires_at: Some("2026-12-31T23:59:59Z".to_owned()),
    }
}

#[test]
fn issuing_over_an_album_sends_the_request_and_forgets_the_passphrases() {
    let mut screen = creating(&[
        "/lib",
        "pw",
        ALBUM_ID,
        "",
        "2026-12-31T23:59:59Z",
        "link pw",
    ]);
    let effects = screen.on_form_key(FormKey::Submit);
    assert_eq!(
        effects,
        vec![Effect::Share(ShareEffect::Create(CreateLinkRequest {
            library: PathBuf::from("/lib"),
            passphrase: Secret::new("pw"),
            scope: ShareScope::Album(ALBUM_ID.parse().expect("uuid")),
            expires_at: Some("2026-12-31T23:59:59Z".to_owned()),
            link_passphrase: Some(Secret::new("link pw")),
        }))]
    );
    assert_eq!(screen.status(), &Status::Working("Issuing the link…"));
    assert!(!screen.is_editing());
    assert!(screen.form().secret(PASSPHRASE).is_empty());
    assert!(screen.form().secret(LINK_PASSPHRASE).is_empty());
}

#[test]
fn an_asset_link_without_expiry_or_passphrase() {
    let mut screen = creating(&["/lib", "pw", "", ASSET_ID]);
    let effects = screen.on_form_key(FormKey::Submit);
    assert_eq!(
        effects,
        vec![Effect::Share(ShareEffect::Create(CreateLinkRequest {
            library: PathBuf::from("/lib"),
            passphrase: Secret::new("pw"),
            scope: ShareScope::Asset(ASSET_ID.parse().expect("uuid")),
            expires_at: None,
            link_passphrase: None,
        }))]
    );
}

#[test]
fn the_issue_form_is_refused_before_any_request() {
    let cases = [
        (
            vec!["", "pw", ALBUM_ID],
            "Enter the library and its passphrase.",
        ),
        (
            vec!["/lib", "pw"],
            "Give an album id or an asset id: exactly one.",
        ),
        (
            vec!["/lib", "pw", ALBUM_ID, ASSET_ID],
            "Give an album id or an asset id: exactly one.",
        ),
        (
            vec!["/lib", "pw", "", "nope"],
            "The asset id \"nope\" is not a UUID.",
        ),
    ];
    for (values, reason) in cases {
        let mut screen = creating(&values);
        assert!(screen.on_form_key(FormKey::Submit).is_empty(), "{values:?}");
        assert_eq!(screen.status(), &Status::Failed(reason.to_owned()));
        assert!(screen.is_editing());
    }
}

#[test]
fn an_issued_link_is_shown_and_its_id_waits_in_the_revoke_form() {
    let mut screen = creating(&["/lib", "pw", ALBUM_ID]);
    screen.on_form_key(FormKey::Submit);
    assert!(
        screen
            .update(ShareAction::Created(Ok(link(false))))
            .is_empty()
    );
    assert_eq!(screen.issued(), Some(&link(false)));
    assert_eq!(
        screen.status(),
        &Status::Done("Share link issued for 3 assets.".to_owned())
    );
    screen.update(ShareAction::BeginRevoke);
    assert_eq!(screen.mode(), ShareMode::Revoke);
    assert!(screen.on_form_key(FormKey::Submit).is_empty(), "asks first");
    assert_eq!(
        screen.on_answer(Answer::Yes),
        vec![Effect::Share(ShareEffect::Revoke {
            opaque_id: OPAQUE.to_owned()
        })]
    );
    screen.update(ShareAction::Revoked(Ok(OPAQUE.to_owned())));
    assert_eq!(screen.issued(), None, "a revoked link is not offered again");
}

#[test]
fn revoking_asks_first_and_no_leaves_the_link_alone() {
    let mut screen = ShareScreen::default();
    screen.update(ShareAction::BeginRevoke);
    for c in OPAQUE.chars() {
        screen.on_form_key(FormKey::Char(c));
    }
    assert!(screen.on_form_key(FormKey::Submit).is_empty());
    assert!(screen.is_confirming());
    assert!(!screen.is_editing());
    assert!(
        matches!(screen.status(), Status::Asking(q) if q.contains(OPAQUE)),
        "{:?}",
        screen.status()
    );
    // Neither issuing nor revoking again replaces the question.
    screen.update(ShareAction::BeginCreate);
    screen.update(ShareAction::BeginRevoke);
    assert!(screen.is_confirming());
    assert_eq!(screen.mode(), ShareMode::Revoke);
    assert!(screen.on_answer(Answer::No).is_empty());
    assert!(!screen.is_confirming());
    assert_eq!(screen.status(), &Status::Idle);
    assert_eq!(screen.form().value(OPAQUE_ID), OPAQUE, "the id stays typed");
    assert!(
        screen.on_answer(Answer::Yes).is_empty(),
        "no question, no revoke"
    );
}

#[test]
fn a_malformed_opaque_id_is_refused_locally() {
    let mut screen = ShareScreen::default();
    screen.update(ShareAction::BeginRevoke);
    for c in "not-an-id".chars() {
        screen.on_form_key(FormKey::Char(c));
    }
    assert!(screen.on_form_key(FormKey::Submit).is_empty());
    assert!(
        matches!(screen.status(), Status::Failed(reason) if reason.contains("not a share link's opaque id")),
        "{:?}",
        screen.status()
    );
}

#[test]
fn refusals_from_the_server_are_shown() {
    let mut screen = ShareScreen::default();
    screen.update(ShareAction::Created(Err("not on the server".to_owned())));
    assert_eq!(
        screen.status(),
        &Status::Failed("Share link not issued: not on the server".to_owned())
    );
    screen.update(ShareAction::Revoked(Err("offline".to_owned())));
    assert_eq!(
        screen.status(),
        &Status::Failed("Share link not revoked: offline".to_owned())
    );
}

#[test]
fn a_new_form_waits_while_a_request_runs() {
    let mut screen = creating(&["/lib", "pw", ALBUM_ID]);
    screen.on_form_key(FormKey::Submit);
    screen.update(ShareAction::BeginRevoke);
    assert_eq!(screen.mode(), ShareMode::Create);
    assert!(!screen.is_editing());
}

#[test]
fn each_failed_effect_reports_its_own_outcome() {
    assert_eq!(
        ShareEffect::Revoke {
            opaque_id: OPAQUE.to_owned()
        }
        .failed("x".to_owned()),
        ShareAction::Revoked(Err("x".to_owned()))
    );
    let create = CreateLinkRequest {
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
        scope: ShareScope::Album(ALBUM_ID.parse().expect("uuid")),
        expires_at: None,
        link_passphrase: None,
    };
    assert_eq!(
        ShareEffect::Create(create).failed("x".to_owned()),
        ShareAction::Created(Err("x".to_owned()))
    );
}

#[test]
fn the_url_and_the_passphrases_never_reach_debug() {
    let mut screen = creating(&["/lib", "lib-secret", ALBUM_ID, "", "", "link-secret"]);
    let effects = screen.on_form_key(FormKey::Submit);
    let action = Action::Share(ShareAction::Created(Ok(link(true))));
    screen.update(ShareAction::Created(Ok(link(true))));
    let printed = format!("{effects:?} {action:?} {screen:?}");
    for secret in ["lib-secret", "link-secret", "fragment-secret"] {
        assert!(!printed.contains(secret), "{secret} in {printed}");
    }
}

#[test]
fn issued_share_converts_with_its_url_as_a_secret() {
    let issued = crate::share::IssuedShare {
        opaque_id: OPAQUE.to_owned(),
        url: URL.to_owned(),
        covered: 3,
        passphrase_protected: true,
        expires_at: Some("2026-12-31T23:59:59Z".to_owned()),
    };
    assert_eq!(IssuedLink::from(issued), link(true));
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn on_share() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    state.update(Action::Open(Screen::Share));
    state
}

#[test]
fn share_links_before_any_request() {
    insta::assert_snapshot!(draw(&on_share()));
}

#[test]
fn share_links_after_issuing_a_protected_link() {
    let mut state = on_share();
    state.update(Action::Share(ShareAction::Created(Ok(link(true)))));
    let frame = draw(&state);
    assert!(frame.contains(URL), "the person copies the URL from here");
    insta::assert_snapshot!(frame);
}

#[test]
fn share_links_asking_whether_to_revoke() {
    let mut state = on_share();
    state.update(Action::Share(ShareAction::Created(Ok(link(false)))));
    state.update(Action::Share(ShareAction::BeginRevoke));
    state.update(Action::Form(FormKey::Submit));
    assert!(state.is_confirming());
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn share_links_revoke_form() {
    let mut state = on_share();
    state.update(Action::Share(ShareAction::BeginRevoke));
    insta::assert_snapshot!(draw(&state));
}
