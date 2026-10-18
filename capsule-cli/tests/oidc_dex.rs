//! `capsule auth login --oidc` round-tripped against a real identity provider: the dex service
//! `capsule-server/compose.yaml` ships (slice `S-N2`).
//!
//! The CLI's own flow — [`oidc::login`] over [`oidc::SdkCeremony`], the loopback listener, the
//! SDK's two legs, the session store — runs unchanged. Only the person at the browser is played
//! by the test: it follows the authorization URL to dex's password form, submits the development
//! account's credentials, and follows dex's redirect back to the CLI's listener.
//!
//! # Gating
//!
//! `mise run test-rust` runs container-free, so this answers *skipped* — one line on stderr, and
//! a pass — unless `CAPSULE_TEST_DEX=1`. Then it needs, already running:
//!
//! ```text
//! podman compose -f capsule-server/compose.yaml --profile oidc up -d dex
//! OIDC_ISSUER=http://127.0.0.1:5556/dex OIDC_CLIENT_ID=capsule OIDC_ALLOW_LOOPBACK_REDIRECT=true \
//!   mise run serve-memory
//! CAPSULE_TEST_DEX=1 cargo nextest run -p capsule-cli --test oidc_dex
//! ```
//!
//! `CAPSULE_ENDPOINT` points the CLI at a server other than `mise run serve-memory`'s.
//! `CAPSULE_TEST_DEX_LOGIN` and `CAPSULE_TEST_DEX_PASSWORD` override the static dex account
//! (`admin@example.com` / `password`).

use std::path::PathBuf;
use std::time::Duration;

use capsule_cli::oidc::{self, CallbackPages, SdkCeremony};
use capsule_cli::remote::{self, LoginStep, RemoteConfig};
use capsule_cli::session::SessionStore;

/// A temp directory that removes itself, so the test needs no `tempfile` dev-dependency.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-s-n2-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("the temp dir is created");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Play the person at the browser: open the authorization URL, sign in at dex's password form,
/// and follow dex back to the CLI's listener.
///
/// dex with one connector sends `/dex/auth` straight to that connector's login page; the form
/// posts back to the page's own URL, and with `skipApprovalScreen` the answer is the redirect to
/// the client. Every redirect is followed, so the last hop is the CLI's loopback listener itself.
async fn sign_in_at_dex(authorization_url: String, login: String, password: String) {
    let browser = reqwest::Client::new();
    let form = browser
        .get(&authorization_url)
        .send()
        .await
        .expect("dex serves the authorization request");
    assert!(
        form.status().is_success(),
        "dex answered the authorization request with {}",
        form.status()
    );
    let login_page = form.url().clone();
    let back = browser
        .post(login_page)
        .form(&[("login", login.as_str()), ("password", password.as_str())])
        .send()
        .await
        .expect("dex takes the password form");
    assert!(
        back.url().as_str().starts_with("http://127.0.0.1:")
            && back.url().path() == oidc::CALLBACK_PATH,
        "dex did not send the browser back to the CLI's listener: {} ({})",
        back.url(),
        back.status()
    );
    assert!(
        back.status().is_success(),
        "the listener answered {}",
        back.status()
    );
}

#[tokio::test]
async fn capsule_auth_login_oidc_round_trips_against_dex() {
    if std::env::var("CAPSULE_TEST_DEX").as_deref() != Ok("1") {
        eprintln!("skipped: set CAPSULE_TEST_DEX=1 with dex and an OIDC-configured server running");
        return;
    }
    let login =
        std::env::var("CAPSULE_TEST_DEX_LOGIN").unwrap_or_else(|_| "admin@example.com".to_owned());
    let password =
        std::env::var("CAPSULE_TEST_DEX_PASSWORD").unwrap_or_else(|_| "password".to_owned());

    let remote = RemoteConfig::from_env();
    let dir = TempDir::new();
    let store = SessionStore::new(dir.0.join("session.json"));
    let ceremony = SdkCeremony::new(&remote, &store).expect("the auth endpoint is a URL");
    let pages = CallbackPages {
        received: "received".to_owned(),
        refused: "refused".to_owned(),
    };

    let mut browser = None;
    let step = oidc::login(
        &ceremony,
        |authorization_url, _| {
            browser = Some(tokio::spawn(sign_in_at_dex(
                authorization_url.to_owned(),
                login,
                password,
            )));
        },
        &pages,
        Duration::from_secs(60),
    )
    .await
    .expect("the sign-in through dex completes");
    browser
        .expect("the authorization URL was shown")
        .await
        .expect("the browser leg finished cleanly");

    assert!(
        matches!(step, LoginStep::Complete),
        "the dex account has no second factor"
    );
    assert!(
        store.load().expect("the store reads").is_some(),
        "the session was persisted"
    );
    // The persisted session is a live one: the server revokes it on logout.
    assert!(
        remote::auth_logout(&remote, &store)
            .await
            .expect("the session the federated sign-in opened is accepted by the server"),
        "there was a session to log out of"
    );
}
