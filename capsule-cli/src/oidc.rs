//! `capsule auth login --oidc`: signing in through the server's identity provider from a
//! terminal.
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
//! [`open_in_browser`] reaches the host through the private `Opener` port (environment and
//! process spawn), so its graphical-session check and each platform's opener are tested with a
//! scripted host; only the real `std::process::Command` spawn is left untested.
//!
//! # Without a browser: `--device`
//!
//! `capsule auth login --oidc --device` is the RFC 8628 device authorization grant over the
//! server's two device legs, for a terminal that cannot receive a redirect — a headless host, an
//! SSH session. [`login_device`] prints the verification address and the user code, never opens
//! a browser and binds no port, and polls at the provider's interval until the person approves
//! on any other device. Its [`DeviceCeremony`] seam is tested the same way, on a paused clock.

use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use capsule_sdk::auth::{AuthClient, OidcAuthorization, OidcDeviceAuthorization, OidcDevicePoll};
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

/// How long the listener pauses after a failed accept before it accepts again.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(50);

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
    /// The person did not approve a device sign-in before its code expired.
    #[error("no sign-in was approved within {} seconds", .waited.as_secs())]
    DeviceTimedOut {
        /// How long the CLI polled for.
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

// ===========================================================================================
// Signing in from another device (RFC 8628, issue #635)
// ===========================================================================================

/// The longest the CLI polls a device sign-in for.
///
/// Fifteen minutes, the server's own ceiling on a device ceremony; the ceremony's `expires_by`
/// caps it further, and is usually what decides.
pub const DEVICE_TIMEOUT: Duration = Duration::from_mins(15);

/// What one device poll came to, from the terminal's side.
pub enum DeviceStep {
    /// Not approved yet: wait the interval and poll again.
    Pending,
    /// Polled too soon, or the provider asked to slow down: raise the interval and poll again.
    SlowDown,
    /// Finished, and settled exactly as a password sign-in is.
    Settled(LoginStep),
}

/// The two server legs of a device sign-in.
///
/// [`SdkCeremony`] is the one the CLI runs; the seam exists so [`login_device`]'s polling — the
/// interval, the slow-downs, the deadline — is tested without a server.
pub trait DeviceCeremony {
    /// Begin a device sign-in.
    fn begin_device(&self) -> impl Future<Output = Result<OidcDeviceAuthorization, RemoteError>>;

    /// Poll it once, persisting a session if one opened.
    fn poll_device(
        &self,
        pending: &OidcDeviceAuthorization,
    ) -> impl Future<Output = Result<DeviceStep, RemoteError>>;
}

impl DeviceCeremony for SdkCeremony<'_> {
    async fn begin_device(&self) -> Result<OidcDeviceAuthorization, RemoteError> {
        Ok(self.client.begin_oidc_device_login().await?)
    }

    async fn poll_device(
        &self,
        pending: &OidcDeviceAuthorization,
    ) -> Result<DeviceStep, RemoteError> {
        match self.client.poll_oidc_device_login(pending).await? {
            OidcDevicePoll::Pending => Ok(DeviceStep::Pending),
            OidcDevicePoll::SlowDown => Ok(DeviceStep::SlowDown),
            OidcDevicePoll::Complete(outcome) => Ok(DeviceStep::Settled(
                remote::settle_login(self.store, outcome).await?,
            )),
        }
    }
}

/// Sign in from another device: begin, show the address and the code, poll until it settles.
///
/// `show` is handed the begun ceremony and how long the CLI will wait, before the first poll;
/// the CLI prints the address and the code there, and never opens a browser. The CLI waits one
/// interval before **every** poll, the first included (RFC 8628 §3.4), and raises the interval by
/// five seconds on every slow-down (§3.5), which keeps it in step with the server without being
/// told the new number. It stops at the ceremony's `expires_by` or `ceiling`, whichever is
/// sooner, and before a poll that would land past it.
///
/// # Errors
///
/// [`OidcLoginError::Remote`] for a refused or failed server leg — the provider's denial and the
/// ceremony's expiry among them — and [`OidcLoginError::DeviceTimedOut`] when it was never
/// approved in time.
#[instrument(skip_all)]
pub async fn login_device<C: DeviceCeremony>(
    ceremony: &C,
    show: impl FnOnce(&OidcDeviceAuthorization, Duration),
    ceiling: Duration,
) -> Result<LoginStep, OidcLoginError> {
    let mut pending = ceremony.begin_device().await?;
    tracing::info!(
        interval_secs = pending.interval_secs,
        "began a device sign-in from the terminal"
    );
    let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
    let bound = wait_bound(pending.expires_by, now, ceiling);
    show(&pending, bound);

    let mut waited = Duration::ZERO;
    loop {
        // Never below a second, whatever a server says: a zero interval is a busy loop.
        let interval = Duration::from_secs(pending.interval_secs.max(1));
        if waited + interval > bound {
            tracing::warn!(
                waited_secs = waited.as_secs(),
                "no device sign-in was approved in time"
            );
            return Err(OidcLoginError::DeviceTimedOut { waited });
        }
        tokio::time::sleep(interval).await;
        waited += interval;
        match ceremony.poll_device(&pending).await? {
            DeviceStep::Pending => {
                tracing::debug!("the device sign-in is not approved yet");
            }
            DeviceStep::SlowDown => {
                pending.slow_down();
                tracing::info!(
                    interval_secs = pending.interval_secs,
                    "the server asked the device sign-in to slow down"
                );
            }
            DeviceStep::Settled(step) => {
                tracing::info!("the device sign-in was approved");
                return Ok(step);
            }
        }
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
                    // problem, not the listener's: keep listening. A short pause first, so an
                    // error that repeats (out of file descriptors) cannot spin this loop.
                    Err(error) => {
                        tracing::debug!(%error, "the OIDC loopback listener failed an accept");
                        tokio::time::sleep(ACCEPT_ERROR_PAUSE).await;
                    }
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
///
/// The URL is the server's, so it is opened only when it parses as an absolute `http` or `https`
/// URL with a host: handed to an opener, a `file:` path, a network-share executable or another
/// URI handler's scheme would be run on the server's say-so. Anything else is left printed and
/// not opened.
pub fn open_in_browser(url: &str) -> bool {
    open_with(&SystemOpener, OpenerPlatform::CURRENT, url)
}

/// Which platform opener [`open_in_browser`] uses: chosen at compile time for the real opener,
/// and named explicitly in tests so every arm runs on every host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenerPlatform {
    /// `open`.
    MacOs,
    /// `rundll32 url.dll,FileProtocolHandler`.
    Windows,
    /// `xdg-open`, only under a graphical session.
    FreeDesktop,
}

impl OpenerPlatform {
    /// The platform this binary was built for.
    const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::FreeDesktop
    };

    /// The opener program and its arguments for `url`.
    fn command(self, url: &str) -> (&'static str, Vec<&str>) {
        match self {
            Self::MacOs => ("open", vec![url]),
            Self::Windows => ("rundll32", vec!["url.dll,FileProtocolHandler", url]),
            Self::FreeDesktop => ("xdg-open", vec![url]),
        }
    }
}

/// What [`open_in_browser`] needs from the host: its environment and a way to start a process.
/// The port exists so the graphical-session check and both spawn outcomes are unit-tested
/// without starting a real opener.
trait Opener {
    /// The environment variable `name`, when set.
    fn var(&self, name: &str) -> Option<std::ffi::OsString>;
    /// Start `program` with `arguments`, detached from this terminal's standard streams, without
    /// waiting for it to exit.
    fn spawn(&self, program: &str, arguments: &[&str]) -> io::Result<()>;
}

/// The real host: the process environment and [`std::process::Command`].
struct SystemOpener;

impl Opener for SystemOpener {
    fn var(&self, name: &str) -> Option<std::ffi::OsString> {
        std::env::var_os(name)
    }

    fn spawn(&self, program: &str, arguments: &[&str]) -> io::Result<()> {
        let mut child = std::process::Command::new(program)
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Whether `opener`'s environment names a graphical session: a non-empty `DISPLAY` or
/// `WAYLAND_DISPLAY`.
fn has_graphical_session(opener: &impl Opener) -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .into_iter()
        .any(|name| opener.var(name).is_some_and(|value| !value.is_empty()))
}

/// [`open_in_browser`] against an explicit host and platform.
fn open_with(opener: &impl Opener, platform: OpenerPlatform, url: &str) -> bool {
    let Some(url) = browser_url(url) else {
        tracing::warn!("the authorization URL is not an http or https URL; it is not opened");
        return false;
    };
    if platform == OpenerPlatform::FreeDesktop && !has_graphical_session(opener) {
        tracing::debug!("no graphical session; the browser is not opened");
        return false;
    }
    let (program, arguments) = platform.command(url.as_str());
    match opener.spawn(program, &arguments) {
        Ok(()) => {
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

/// `url` parsed, when it is an absolute `http` or `https` URL with a host — the only kind
/// [`open_in_browser`] hands to the platform's opener.
fn browser_url(url: &str) -> Option<url::Url> {
    let parsed = url::Url::parse(url).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some()).then_some(parsed)
}

#[cfg(test)]
mod tests;
