//! `capsule auth login --oidc`: signing in through the server's identity provider from a
//! terminal (slice `S-N2`).
//!
//! # The flow
//!
//! 1. Bind a listener on an ephemeral port of the loopback IP literal `127.0.0.1` — never
//!    `localhost`, which RFC 8252 §8.3 warns can resolve off-box, and which the server's
//!    redirect policy does not admit — and name `http://127.0.0.1:{port}/callback` as the
//!    redirect (RFC 8252 §7.3).
//! 2. [`AuthClient::begin_oidc_login`] with it. The server admits a loopback redirect only when
//!    `OIDC_ALLOW_LOOPBACK_REDIRECT=true`, which is **off** by default; a refusal is
//!    `error.auth.oidc_redirect_invalid`, and the terminal tells the operator which variable to
//!    set.
//! 3. Print the authorization URL, and — unless `--no-browser` — hand it to the platform's opener.
//!    The URL is printed either way: a terminal on another machine than the browser, a headless
//!    host, or an opener that fails all leave the person a working path.
//! 4. Wait for the provider's redirect on the listener, for at most [`LISTENER_TIMEOUT`] and
//!    never past the ceremony's own deadline.
//! 5. [`AuthClient::complete_oidc_login`] with the redirect's `state` and `code`, and persist the
//!    session exactly as a password sign-in does — second factor included.
//!
//! # What the listener accepts
//!
//! Only `GET /callback` carrying the `state` this ceremony began. Anything else — a browser's
//! favicon fetch, a speculative connection that sends nothing, a request another local process
//! made with some other `state` — is answered and ignored, and the listener keeps waiting. A
//! foreign `state` could not complete anyway (the SDK and the server both refuse it, issue #517),
//! but ignoring it rather than failing on it is what stops a local process from cutting a
//! sign-in short by racing the browser to the port.
//!
//! A redirect carrying the provider's `error` (RFC 6749 §4.1.2.1) for this ceremony's `state`
//! ends the wait: the person declined, or the provider refused, and nothing more will arrive.
//!
//! # Testing seam
//!
//! The two server legs are a [`Ceremony`]; [`login`] drives any. [`SdkCeremony`] is the real
//! one, over `capsule-sdk`. The listener and the whole orchestration are exercised against a
//! scripted ceremony and a scripted browser in this module's tests; the round trip against a real
//! provider is `tests/oidc_dex.rs`, against the dex service `capsule-server/compose.yaml` ships.

use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use capsule_sdk::auth::{AuthClient, OidcAuthorization};
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use secrecy::ExposeSecret as _;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tracing::instrument;

use crate::remote::{self, LoginStep, RemoteConfig, RemoteError};
use crate::session::SessionStore;

/// The path the provider redirects to on the loopback listener.
pub const CALLBACK_PATH: &str = "/callback";

/// The longest the CLI waits for the provider's redirect.
///
/// Five minutes: long enough to sign in at a provider that asks for a password and a second
/// factor, short enough that an abandoned terminal is not a port held open for the ceremony's
/// whole ten-minute life. The ceremony's own deadline caps it further when it is nearer.
pub const LISTENER_TIMEOUT: Duration = Duration::from_secs(300);

/// The longest one connection may take to send its request line and headers.
///
/// Browsers open speculative connections that send nothing; each is dropped after this, and
/// none of them holds up the real redirect, because connections are read concurrently.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of a request the listener reads before it gives up on it. A redirect is one request
/// line and a few headers; the query a provider sends is well under this.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// The most connections the listener reads at once. Beyond it a new connection is closed
/// unanswered: a browser opens a handful, and nothing legitimate opens more.
const MAX_IN_FLIGHT: usize = 16;

/// Why an OIDC sign-in from the terminal did not finish.
#[derive(Debug, Error)]
pub enum OidcLoginError {
    /// A server leg failed: the SDK's typed refusal, or a store failure persisting the session.
    #[error(transparent)]
    Remote(#[from] RemoteError),
    /// The loopback listener could not be bound.
    #[error("the loopback listener could not be bound: {0}")]
    Listener(#[source] io::Error),
    /// No redirect for this ceremony arrived in time.
    #[error("no sign-in arrived within {} seconds", .waited.as_secs())]
    TimedOut {
        /// How long the listener waited.
        waited: Duration,
    },
    /// The provider's redirect carried an `error` instead of a `code` (RFC 6749 §4.1.2.1).
    #[error("the identity provider answered {error}")]
    Denied {
        /// The provider's `error` parameter, e.g. `access_denied`.
        error: String,
        /// The provider's `error_description`, when it sent one. Provider-authored English.
        description: Option<String>,
    },
}

/// The two server legs of a sign-in through the identity provider.
///
/// [`SdkCeremony`] is the one the CLI runs; the seam exists so [`login`]'s orchestration — the
/// listener, the deadline, which redirect is accepted — is tested without a server.
pub trait Ceremony {
    /// Begin a ceremony that will send the person back to `redirect_uri`.
    fn begin(
        &self,
        redirect_uri: &str,
    ) -> impl Future<Output = Result<OidcAuthorization, RemoteError>>;

    /// Finish `pending` with what the redirect carried, persisting a session if one opened.
    fn complete(
        &self,
        pending: &OidcAuthorization,
        state: &str,
        code: &str,
    ) -> impl Future<Output = Result<LoginStep, RemoteError>>;
}

/// The real [`Ceremony`]: `capsule-sdk`'s two OIDC legs, persisting into the CLI's store.
pub struct SdkCeremony<'a> {
    client: AuthClient,
    store: &'a SessionStore,
}

impl<'a> SdkCeremony<'a> {
    /// A ceremony against `remote`'s auth surface, persisting into `store`.
    ///
    /// # Errors
    ///
    /// [`RemoteError::Auth`] when the auth endpoint is not a usable URL.
    pub fn new(remote: &RemoteConfig, store: &'a SessionStore) -> Result<Self, RemoteError> {
        Ok(Self {
            client: AuthClient::new(&remote.auth_endpoint)?,
            store,
        })
    }
}

impl Ceremony for SdkCeremony<'_> {
    async fn begin(&self, redirect_uri: &str) -> Result<OidcAuthorization, RemoteError> {
        Ok(self.client.begin_oidc_login(redirect_uri).await?)
    }

    async fn complete(
        &self,
        pending: &OidcAuthorization,
        state: &str,
        code: &str,
    ) -> Result<LoginStep, RemoteError> {
        let outcome = self
            .client
            .complete_oidc_login(pending, state, code)
            .await?;
        remote::settle_login(self.store, outcome).await
    }
}

/// The two pages the listener shows the browser once it has the redirect.
///
/// Localized by the caller: the browser tab is the one place this flow speaks to the person
/// outside the terminal.
#[derive(Debug, Clone)]
pub struct CallbackPages {
    /// Shown when a `code` arrived: the terminal takes it from here.
    pub received: String,
    /// Shown when the provider sent an `error`: the terminal says what went wrong.
    pub refused: String,
}

/// What the listener took from the provider's redirect.
#[derive(Debug, PartialEq, Eq)]
pub enum Redirect {
    /// The provider issued a code for this ceremony.
    Code {
        /// The `state` the redirect echoed — this ceremony's.
        state: String,
        /// The authorization code.
        code: String,
    },
    /// The provider answered with an error for this ceremony.
    Denied {
        /// The `error` parameter.
        error: String,
        /// The `error_description` parameter, when present.
        description: Option<String>,
    },
}

/// A listener on an ephemeral loopback port, waiting for one redirect.
pub struct LoopbackListener {
    listener: TcpListener,
    redirect_uri: String,
}

impl LoopbackListener {
    /// Bind an ephemeral port on `127.0.0.1`.
    ///
    /// # Errors
    ///
    /// The bind's own I/O error.
    pub async fn bind() -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        tracing::debug!(port, "the OIDC loopback listener is bound");
        Ok(Self {
            listener,
            redirect_uri,
        })
    }

    /// The redirect URI to name at the authorize: `http://127.0.0.1:{port}/callback`.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait — unbounded; the caller bounds it — for the redirect carrying `expected_state`.
    ///
    /// Every connection is answered: the redirect with `pages`, anything else with a bare
    /// status. Connections are read concurrently, so one that sends nothing does not hold up the
    /// one that carries the redirect.
    pub async fn receive(&self, expected_state: &str, pages: &CallbackPages) -> Redirect {
        let mut in_flight = FuturesUnordered::new();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, peer)) if in_flight.len() < MAX_IN_FLIGHT => {
                        tracing::trace!(%peer, "the OIDC loopback listener accepted a connection");
                        in_flight.push(read_request(stream));
                    }
                    Ok((_, peer)) => {
                        tracing::debug!(%peer, "the OIDC loopback listener is saturated; closed a connection");
                    }
                    // A failed accept (a connection reset before it was taken) is the client's
                    // problem, not the listener's: keep listening.
                    Err(error) => tracing::debug!(%error, "the OIDC loopback listener failed an accept"),
                },
                Some((mut stream, target)) = in_flight.next() => {
                    let verdict = target.as_deref().map(|target| judge(target, expected_state));
                    let (status, body) = match &verdict {
                        Some(Verdict::Accepted(Redirect::Code { .. })) => ("200 OK", pages.received.as_str()),
                        Some(Verdict::Accepted(Redirect::Denied { .. })) => ("200 OK", pages.refused.as_str()),
                        Some(Verdict::NotCallback) => ("404 Not Found", ""),
                        Some(Verdict::ForeignState) | None => ("400 Bad Request", ""),
                    };
                    respond(&mut stream, status, body).await;
                    match verdict {
                        Some(Verdict::Accepted(redirect)) => {
                            tracing::info!("the OIDC loopback listener received the provider's redirect");
                            return redirect;
                        }
                        Some(Verdict::ForeignState) => {
                            tracing::warn!("the OIDC loopback listener ignored a callback for another ceremony");
                        }
                        Some(Verdict::NotCallback) | None => {}
                    }
                }
            }
        }
    }
}

/// What one request to the listener amounts to.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The redirect for this ceremony.
    Accepted(Redirect),
    /// Not `GET /callback`.
    NotCallback,
    /// `GET /callback`, but for some other `state`, or with neither a `code` nor an `error`.
    ForeignState,
}

/// Read one request's target from `stream`: `Some("/callback?…")` for a `GET`, `None` for
/// anything malformed, oversized, slow or not a `GET`.
async fn read_request(mut stream: TcpStream) -> (TcpStream, Option<String>) {
    let read = tokio::time::timeout(REQUEST_READ_TIMEOUT, async {
        let mut buffer = Vec::with_capacity(1024);
        let mut chunk = [0_u8; 1024];
        loop {
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                return Some(buffer);
            }
            if buffer.len() >= MAX_REQUEST_BYTES {
                return None;
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return None,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        }
    })
    .await;
    let target = match read {
        Ok(Some(buffer)) => request_target(&buffer),
        _ => None,
    };
    (stream, target)
}

/// The target of a `GET` request line, e.g. `/callback?state=…&code=…`.
fn request_target(request: &[u8]) -> Option<String> {
    let line_end = request.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(&request[..line_end]).ok()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    (method == "GET" && version.starts_with("HTTP/1.") && parts.next().is_none())
        .then(|| target.to_owned())
}

/// Judge a request target against the ceremony that is waiting.
fn judge(target: &str, expected_state: &str) -> Verdict {
    if !target.starts_with('/') {
        return Verdict::NotCallback;
    }
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Verdict::NotCallback;
    };
    if url.path() != CALLBACK_PATH {
        return Verdict::NotCallback;
    }
    // The first of each parameter: a provider sends each once, and a repeated one is no reason
    // to read further.
    let parameter = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if parameter("state").as_deref() != Some(expected_state) {
        return Verdict::ForeignState;
    }
    if let Some(code) = parameter("code").filter(|code| !code.is_empty()) {
        return Verdict::Accepted(Redirect::Code {
            state: expected_state.to_owned(),
            code,
        });
    }
    match parameter("error").filter(|error| !error.is_empty()) {
        Some(error) => Verdict::Accepted(Redirect::Denied {
            error,
            description: parameter("error_description"),
        }),
        None => Verdict::ForeignState,
    }
}

/// Answer one connection and close it. Best effort: a browser that went away is not an error.
async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let page = if body.is_empty() {
        String::new()
    } else {
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Capsule</title></head>\
             <body><p>{}</p></body></html>",
            escape_html(body)
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    if let Err(error) = stream.write_all(response.as_bytes()).await {
        tracing::debug!(%error, "the OIDC loopback listener could not answer a connection");
    }
    let _ = stream.shutdown().await;
}

/// Escape the characters that would make a message markup.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// How long to wait for the redirect: [`LISTENER_TIMEOUT`], or less when the ceremony's
/// `expires_by` (absolute Unix seconds) is nearer than that.
#[must_use]
pub fn wait_bound(expires_by: u64, now_unix: u64, ceiling: Duration) -> Duration {
    ceiling.min(Duration::from_secs(expires_by.saturating_sub(now_unix)))
}

/// Sign in through the identity provider: bind, begin, show the URL, wait, complete.
///
/// `show` is handed the authorization URL once the ceremony has begun and before the wait
/// starts; the CLI prints it and opens the browser there. `ceiling` is [`LISTENER_TIMEOUT`]
/// outside tests.
///
/// # Errors
///
/// [`OidcLoginError::Listener`] when no loopback port can be bound,
/// [`OidcLoginError::Remote`] for a refused or failed server leg,
/// [`OidcLoginError::Denied`] when the provider redirected with an error, and
/// [`OidcLoginError::TimedOut`] when nothing arrived in time.
#[instrument(skip_all)]
pub async fn login<C: Ceremony>(
    ceremony: &C,
    show: impl FnOnce(&str, Duration),
    pages: &CallbackPages,
    ceiling: Duration,
) -> Result<LoginStep, OidcLoginError> {
    let listener = LoopbackListener::bind()
        .await
        .map_err(OidcLoginError::Listener)?;
    let pending = ceremony.begin(listener.redirect_uri()).await?;
    tracing::info!("began an OIDC sign-in from the terminal");

    let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
    let waited = wait_bound(pending.expires_by, now, ceiling);
    show(&pending.authorization_url, waited);

    let redirect = tokio::time::timeout(
        waited,
        listener.receive(pending.state.expose_secret(), pages),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            waited_secs = waited.as_secs(),
            "no OIDC redirect arrived in time"
        );
        OidcLoginError::TimedOut { waited }
    })?;
    // Closed before the completing leg: nothing more should reach this port.
    drop(listener);

    match redirect {
        Redirect::Code { state, code } => Ok(ceremony.complete(&pending, &state, &code).await?),
        Redirect::Denied { error, description } => {
            tracing::warn!(%error, "the identity provider redirected with an error");
            Err(OidcLoginError::Denied { error, description })
        }
    }
}

/// Hand `url` to the platform's opener, returning whether one was started.
///
/// `open` on macOS, the URL handler on Windows, `xdg-open` elsewhere — and on Linux and the BSDs
/// only under a graphical session (`DISPLAY` or `WAYLAND_DISPLAY`): without one, `xdg-open` falls
/// back to a text-mode browser that would take over this very terminal. The child is reaped on a
/// background thread; whether it then finds a browser is not observable, which is why the URL is
/// always printed too.
pub fn open_in_browser(url: &str) -> bool {
    let (program, arguments): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("rundll32", vec!["url.dll,FileProtocolHandler", url])
    } else {
        let graphical = std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
        if !graphical {
            tracing::debug!("no graphical session; the browser is not opened");
            return false;
        }
        ("xdg-open", vec![url])
    };
    match std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            tracing::debug!(
                program,
                "handed the authorization URL to the platform opener"
            );
            true
        }
        Err(error) => {
            tracing::debug!(program, %error, "the platform opener could not be started");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use secrecy::SecretString;

    use super::*;

    const STATE: &str = "the-ceremony-state";

    fn pages() -> CallbackPages {
        CallbackPages {
            received: "Signed in <ok>".to_owned(),
            refused: "Refused".to_owned(),
        }
    }

    #[test]
    fn only_a_get_request_line_has_a_target() {
        assert_eq!(
            request_target(b"GET /callback?code=1 HTTP/1.1\r\nHost: x\r\n\r\n").as_deref(),
            Some("/callback?code=1")
        );
        assert_eq!(request_target(b"POST /callback HTTP/1.1\r\n\r\n"), None);
        assert_eq!(request_target(b"GET /callback\r\n\r\n"), None);
        assert_eq!(request_target(b"GET /callback SPDY/3\r\n\r\n"), None);
        assert_eq!(request_target(b"GET /a b HTTP/1.1\r\n\r\n"), None);
        assert_eq!(request_target(b"\xff\xfe HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn the_callback_for_this_state_is_accepted_and_nothing_else_is() {
        assert_eq!(
            judge(&format!("/callback?state={STATE}&code=c%2B1"), STATE),
            Verdict::Accepted(Redirect::Code {
                state: STATE.to_owned(),
                code: "c+1".to_owned(),
            })
        );
        assert_eq!(
            judge(
                &format!("/callback?code=c&state={STATE}&iss=https%3A%2F%2Fidp"),
                STATE
            ),
            Verdict::Accepted(Redirect::Code {
                state: STATE.to_owned(),
                code: "c".to_owned(),
            })
        );
        assert_eq!(
            judge(
                &format!(
                    "/callback?state={STATE}&error=access_denied&error_description=no%20thanks"
                ),
                STATE
            ),
            Verdict::Accepted(Redirect::Denied {
                error: "access_denied".to_owned(),
                description: Some("no thanks".to_owned()),
            })
        );
        assert_eq!(judge("/favicon.ico", STATE), Verdict::NotCallback);
        assert_eq!(
            judge("/callback/extra?state=x", STATE),
            Verdict::NotCallback
        );
        assert_eq!(judge("http://evil/callback", STATE), Verdict::NotCallback);
        assert_eq!(
            judge("/callback?state=someone-else&code=c", STATE),
            Verdict::ForeignState
        );
        assert_eq!(judge("/callback?code=c", STATE), Verdict::ForeignState);
        assert_eq!(
            judge(&format!("/callback?state={STATE}"), STATE),
            Verdict::ForeignState
        );
        assert_eq!(
            judge(&format!("/callback?state={STATE}&code="), STATE),
            Verdict::ForeignState
        );
    }

    #[test]
    fn the_wait_is_capped_by_the_ceremony_deadline() {
        assert_eq!(
            wait_bound(1_000 + 600, 1_000, LISTENER_TIMEOUT),
            LISTENER_TIMEOUT
        );
        assert_eq!(
            wait_bound(1_000 + 60, 1_000, LISTENER_TIMEOUT),
            Duration::from_secs(60)
        );
        assert_eq!(wait_bound(900, 1_000, LISTENER_TIMEOUT), Duration::ZERO);
    }

    #[test]
    fn a_page_cannot_carry_markup() {
        assert_eq!(
            escape_html("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }

    /// One raw HTTP exchange with the listener: what a browser does.
    async fn get(redirect_uri: &str, target: &str) -> String {
        let authority = redirect_uri
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .expect("a loopback redirect URI");
        let mut stream = TcpStream::connect(authority)
            .await
            .expect("the listener accepts");
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
            .await
            .expect("the request is written");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("the response is read");
        response
    }

    #[tokio::test]
    async fn the_listener_binds_loopback_and_ignores_what_is_not_the_redirect() {
        let listener = LoopbackListener::bind()
            .await
            .expect("a loopback port binds");
        let redirect_uri = listener.redirect_uri().to_owned();
        assert!(
            redirect_uri.starts_with("http://127.0.0.1:"),
            "{redirect_uri}"
        );
        assert!(redirect_uri.ends_with(CALLBACK_PATH), "{redirect_uri}");

        let browser = tokio::spawn({
            let redirect_uri = redirect_uri.clone();
            async move {
                // A speculative connection that never sends a byte must not block the redirect.
                let authority = redirect_uri
                    .strip_prefix("http://")
                    .and_then(|rest| rest.split('/').next())
                    .expect("authority")
                    .to_owned();
                let idle = TcpStream::connect(&authority).await.expect("connects");
                let favicon = get(&redirect_uri, "/favicon.ico").await;
                let foreign = get(&redirect_uri, "/callback?state=another&code=stolen").await;
                let real = get(
                    &redirect_uri,
                    &format!("/callback?state={STATE}&code=the-code"),
                )
                .await;
                drop(idle);
                (favicon, foreign, real)
            }
        });

        let redirect =
            tokio::time::timeout(Duration::from_secs(5), listener.receive(STATE, &pages()))
                .await
                .expect("the redirect arrives");
        assert_eq!(
            redirect,
            Redirect::Code {
                state: STATE.to_owned(),
                code: "the-code".to_owned(),
            }
        );
        let (favicon, foreign, real) = browser.await.expect("the browser finishes");
        assert!(favicon.starts_with("HTTP/1.1 404"), "{favicon}");
        assert!(foreign.starts_with("HTTP/1.1 400"), "{foreign}");
        assert!(real.starts_with("HTTP/1.1 200"), "{real}");
        assert!(real.contains("Signed in &lt;ok&gt;"), "{real}");
        assert!(real.contains("Cache-Control: no-store"), "{real}");
    }

    /// A ceremony the test scripts: it records what it was asked and answers as told.
    struct Scripted {
        expires_in: u64,
        begun_with: Mutex<Option<String>>,
        completed_with: Mutex<Option<(String, String)>>,
    }

    impl Scripted {
        fn new(expires_in: u64) -> Self {
            Self {
                expires_in,
                begun_with: Mutex::new(None),
                completed_with: Mutex::new(None),
            }
        }
    }

    impl Ceremony for Scripted {
        async fn begin(&self, redirect_uri: &str) -> Result<OidcAuthorization, RemoteError> {
            *self.begun_with.lock().expect("unpoisoned") = Some(redirect_uri.to_owned());
            let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
            Ok(OidcAuthorization {
                authorization_url: format!(
                    "https://idp.test/authorize?redirect_uri={redirect_uri}&state={STATE}"
                ),
                state: SecretString::from(STATE),
                binding: SecretString::from("binding"),
                expires_by: now + self.expires_in,
            })
        }

        async fn complete(
            &self,
            pending: &OidcAuthorization,
            state: &str,
            code: &str,
        ) -> Result<LoginStep, RemoteError> {
            assert_eq!(pending.state.expose_secret(), STATE);
            *self.completed_with.lock().expect("unpoisoned") =
                Some((state.to_owned(), code.to_owned()));
            Ok(LoginStep::Complete)
        }
    }

    /// The redirect URI the authorization URL names, as a browser would follow it.
    fn redirect_of(authorization_url: &str) -> String {
        let url = url::Url::parse(authorization_url).expect("a URL");
        url.query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .map(|(_, value)| value.into_owned())
            .expect("a redirect_uri")
    }

    #[tokio::test]
    async fn a_sign_in_completes_with_the_code_the_browser_carried_back() {
        let ceremony = Scripted::new(600);
        let step = login(
            &ceremony,
            |authorization_url, waited| {
                assert_eq!(waited, LISTENER_TIMEOUT);
                let redirect_uri = redirect_of(authorization_url);
                tokio::spawn(async move {
                    get(
                        &redirect_uri,
                        &format!("/callback?state={STATE}&code=granted"),
                    )
                    .await
                });
            },
            &pages(),
            LISTENER_TIMEOUT,
        )
        .await
        .expect("the sign-in completes");
        assert!(matches!(step, LoginStep::Complete));
        let begun = ceremony.begun_with.lock().expect("unpoisoned").clone();
        assert!(
            begun
                .as_deref()
                .is_some_and(|uri| uri.starts_with("http://127.0.0.1:")),
            "{begun:?}"
        );
        assert_eq!(
            ceremony.completed_with.lock().expect("unpoisoned").clone(),
            Some((STATE.to_owned(), "granted".to_owned()))
        );
    }

    #[tokio::test]
    async fn a_provider_error_ends_the_wait_without_completing() {
        let ceremony = Scripted::new(600);
        let error = login(
            &ceremony,
            |authorization_url, _| {
                let redirect_uri = redirect_of(authorization_url);
                tokio::spawn(async move {
                    get(
                        &redirect_uri,
                        &format!("/callback?state={STATE}&error=access_denied"),
                    )
                    .await
                });
            },
            &pages(),
            LISTENER_TIMEOUT,
        )
        .await
        .expect_err("a refusal is an error");
        assert!(
            matches!(&error, OidcLoginError::Denied { error, description: None } if error == "access_denied"),
            "{error:?}"
        );
        assert!(
            ceremony
                .completed_with
                .lock()
                .expect("unpoisoned")
                .is_none()
        );
    }

    #[tokio::test]
    async fn nothing_arriving_is_a_timeout_and_a_foreign_state_does_not_end_the_wait() {
        let ceremony = Scripted::new(600);
        let error = login(
            &ceremony,
            |authorization_url, _| {
                let redirect_uri = redirect_of(authorization_url);
                tokio::spawn(async move {
                    get(&redirect_uri, "/callback?state=forged&error=access_denied").await
                });
            },
            &pages(),
            Duration::from_millis(500),
        )
        .await
        .expect_err("nothing for this ceremony arrived");
        assert!(
            matches!(error, OidcLoginError::TimedOut { .. }),
            "{error:?}"
        );
        assert!(
            ceremony
                .completed_with
                .lock()
                .expect("unpoisoned")
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_ceremony_deadline_bounds_the_wait() {
        // A ceremony already at its deadline: the wait is zero, whatever the ceiling.
        let ceremony = Scripted::new(0);
        let mut shown = None;
        let error = login(
            &ceremony,
            |_, waited| shown = Some(waited),
            &pages(),
            LISTENER_TIMEOUT,
        )
        .await
        .expect_err("an expired ceremony cannot be waited on");
        assert!(
            matches!(error, OidcLoginError::TimedOut { .. }),
            "{error:?}"
        );
        assert_eq!(shown, Some(Duration::ZERO));
    }
}
