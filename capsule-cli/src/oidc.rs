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
    fn only_an_http_or_https_url_is_handed_to_the_opener() {
        for openable in [
            "https://idp.test/authorize?state=s&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcallback",
            "http://127.0.0.1:5556/dex/auth",
            "HTTPS://IdP.Test/authorize",
        ] {
            assert!(browser_url(openable).is_some(), "{openable}");
        }
        for refused in [
            "file:///etc/passwd",
            "file://server/share/payload.exe",
            "\\\\server\\share\\payload.exe",
            "C:\\Windows\\System32\\calc.exe",
            "/usr/bin/xterm",
            "ms-settings:privacy",
            "javascript:alert(1)",
            "smb://server/share",
            "ftp://idp.test/authorize",
            "-a Calculator",
            "",
        ] {
            assert!(browser_url(refused).is_none(), "{refused}");
            assert!(!open_in_browser(refused), "{refused}");
        }
    }

    /// A scripted [`Opener`]: a fixed environment, a fixed spawn outcome, and a record of every
    /// spawn it was asked for.
    struct ScriptedOpener {
        env: Vec<(&'static str, &'static str)>,
        spawn_fails: bool,
        spawned: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl ScriptedOpener {
        fn new(env: &[(&'static str, &'static str)], spawn_fails: bool) -> Self {
            Self {
                env: env.to_vec(),
                spawn_fails,
                spawned: Mutex::new(Vec::new()),
            }
        }

        fn spawned(&self) -> Vec<(String, Vec<String>)> {
            self.spawned.lock().unwrap().clone()
        }
    }

    impl Opener for ScriptedOpener {
        fn var(&self, name: &str) -> Option<std::ffi::OsString> {
            self.env
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        }

        fn spawn(&self, program: &str, arguments: &[&str]) -> io::Result<()> {
            self.spawned.lock().unwrap().push((
                program.to_owned(),
                arguments
                    .iter()
                    .map(|&argument| argument.to_owned())
                    .collect(),
            ));
            if self.spawn_fails {
                Err(io::Error::new(io::ErrorKind::NotFound, "no such program"))
            } else {
                Ok(())
            }
        }
    }

    const AUTHORIZE: &str = "https://idp.test/authorize?state=s";

    #[test]
    fn a_graphical_session_is_a_non_empty_display_or_wayland_display() {
        for (env, graphical) in [
            (&[][..], false),
            (&[("DISPLAY", "")][..], false),
            (&[("WAYLAND_DISPLAY", "")][..], false),
            (&[("DISPLAY", ""), ("WAYLAND_DISPLAY", "")][..], false),
            (&[("TERM", "xterm")][..], false),
            (&[("DISPLAY", ":0")][..], true),
            (&[("WAYLAND_DISPLAY", "wayland-0")][..], true),
            (
                &[("DISPLAY", ""), ("WAYLAND_DISPLAY", "wayland-0")][..],
                true,
            ),
        ] {
            let opener = ScriptedOpener::new(env, false);
            assert_eq!(has_graphical_session(&opener), graphical, "{env:?}");
        }
    }

    #[test]
    fn without_a_graphical_session_xdg_open_is_never_spawned() {
        let opener = ScriptedOpener::new(&[("DISPLAY", "")], false);
        assert!(!open_with(&opener, OpenerPlatform::FreeDesktop, AUTHORIZE));
        assert!(opener.spawned().is_empty());
    }

    #[test]
    fn each_platform_spawns_its_opener_with_the_url() {
        let url = browser_url(AUTHORIZE).unwrap().to_string();
        for (platform, program, arguments) in [
            (OpenerPlatform::MacOs, "open", vec![url.clone()]),
            (
                OpenerPlatform::Windows,
                "rundll32",
                vec!["url.dll,FileProtocolHandler".to_owned(), url.clone()],
            ),
            (OpenerPlatform::FreeDesktop, "xdg-open", vec![url.clone()]),
        ] {
            // No graphical session: only the freedesktop opener consults one.
            let env: &[_] = if platform == OpenerPlatform::FreeDesktop {
                &[("WAYLAND_DISPLAY", "wayland-0")]
            } else {
                &[]
            };
            let opener = ScriptedOpener::new(env, false);
            assert!(open_with(&opener, platform, AUTHORIZE), "{platform:?}");
            assert_eq!(
                opener.spawned(),
                vec![(program.to_owned(), arguments)],
                "{platform:?}"
            );
        }
    }

    #[test]
    fn an_opener_that_cannot_be_started_is_reported_as_not_opened() {
        for platform in [
            OpenerPlatform::MacOs,
            OpenerPlatform::Windows,
            OpenerPlatform::FreeDesktop,
        ] {
            let opener = ScriptedOpener::new(&[("DISPLAY", ":0")], true);
            assert!(!open_with(&opener, platform, AUTHORIZE), "{platform:?}");
            assert_eq!(opener.spawned().len(), 1, "{platform:?}");
        }
    }

    #[test]
    fn a_refused_url_is_never_spawned_on_any_platform() {
        for platform in [
            OpenerPlatform::MacOs,
            OpenerPlatform::Windows,
            OpenerPlatform::FreeDesktop,
        ] {
            let opener = ScriptedOpener::new(&[("DISPLAY", ":0")], false);
            assert!(!open_with(&opener, platform, "file:///etc/passwd"));
            assert!(opener.spawned().is_empty(), "{platform:?}");
        }
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

    /// A connected pair: the listener's side and the client's.
    async fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("a loopback port binds");
        let address = listener.local_addr().expect("a bound address");
        let (accepted, client) = tokio::join!(listener.accept(), TcpStream::connect(address));
        (
            accepted.expect("the listener accepts").0,
            client.expect("the client connects"),
        )
    }

    /// The authority (`127.0.0.1:{port}`) of a loopback redirect URI.
    fn authority_of(redirect_uri: &str) -> String {
        redirect_uri
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .expect("a loopback redirect URI")
            .to_owned()
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_sends_nothing_is_dropped_after_the_read_timeout() {
        let (server, _client) = connected_pair().await;
        let started = tokio::time::Instant::now();
        let (_, target) = read_request(server).await;
        assert_eq!(target, None);
        assert!(
            started.elapsed() >= REQUEST_READ_TIMEOUT,
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_connection_is_answered_400_once_the_read_timeout_passes() {
        let listener = LoopbackListener::bind()
            .await
            .expect("a loopback port binds");
        let authority = authority_of(listener.redirect_uri());
        let silent = async {
            let mut stream = TcpStream::connect(&authority).await.expect("connects");
            let started = tokio::time::Instant::now();
            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .await
                .expect("the response is read");
            (response, started.elapsed())
        };
        let pages = pages();
        tokio::select! {
            redirect = listener.receive(STATE, &pages) => panic!("nothing was sent: {redirect:?}"),
            (response, waited) = silent => {
                assert!(response.starts_with("HTTP/1.1 400"), "{response}");
                assert!(waited >= REQUEST_READ_TIMEOUT, "{waited:?}");
            }
        }
    }

    #[tokio::test]
    async fn a_request_that_reaches_the_size_cap_is_dropped_without_waiting() {
        let (server, mut client) = connected_pair().await;
        // A request line and headers that never end, exactly at the cap.
        let mut request = b"GET /callback?state=".to_vec();
        request.resize(MAX_REQUEST_BYTES, b'a');
        client
            .write_all(&request)
            .await
            .expect("the request is written");
        let (_, target) = tokio::time::timeout(REQUEST_READ_TIMEOUT / 2, read_request(server))
            .await
            .expect("the cap ends the read before the read timeout");
        assert_eq!(target, None);

        // The same request, one byte short of the cap and then terminated, is read.
        let (server, mut client) = connected_pair().await;
        let mut request = format!("GET /callback?state={STATE}&code=").into_bytes();
        request.resize(MAX_REQUEST_BYTES - b" HTTP/1.1\r\n\r\n".len() - 1, b'a');
        request.extend_from_slice(b" HTTP/1.1\r\n\r\n");
        client
            .write_all(&request)
            .await
            .expect("the request is written");
        let (_, target) = read_request(server).await;
        assert!(
            target
                .as_deref()
                .is_some_and(|target| target.starts_with("/callback?state=")),
            "{target:?}"
        );
    }

    #[tokio::test]
    async fn an_oversized_request_is_answered_400_and_the_wait_goes_on() {
        let listener = LoopbackListener::bind()
            .await
            .expect("a loopback port binds");
        let redirect_uri = listener.redirect_uri().to_owned();
        let browser = async {
            let mut stream = TcpStream::connect(authority_of(&redirect_uri))
                .await
                .expect("connects");
            let mut request = format!("GET /callback?state={STATE}&code=").into_bytes();
            request.resize(MAX_REQUEST_BYTES, b'a');
            stream
                .write_all(&request)
                .await
                .expect("the request is written");
            let mut oversized = String::new();
            stream
                .read_to_string(&mut oversized)
                .await
                .expect("the response is read");
            let real = get(
                &redirect_uri,
                &format!("/callback?state={STATE}&code=after"),
            )
            .await;
            (oversized, real)
        };
        let (redirect, (oversized, real)) = tokio::time::timeout(
            Duration::from_secs(5),
            futures::future::join(listener.receive(STATE, &pages()), browser),
        )
        .await
        .expect("the oversized request does not hold up the redirect");
        assert!(oversized.starts_with("HTTP/1.1 400"), "{oversized}");
        assert!(real.starts_with("HTTP/1.1 200"), "{real}");
        assert_eq!(
            redirect,
            Redirect::Code {
                state: STATE.to_owned(),
                code: "after".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn past_the_in_flight_cap_a_connection_is_closed_unanswered() {
        let listener = LoopbackListener::bind()
            .await
            .expect("a loopback port binds");
        let redirect_uri = listener.redirect_uri().to_owned();
        let authority = authority_of(&redirect_uri);
        let browser = async {
            // Fill every in-flight slot with a connection that sends nothing. Each is held open
            // until the listener has answered it, so the slots stay taken.
            let mut idle = Vec::with_capacity(MAX_IN_FLIGHT);
            for _ in 0..MAX_IN_FLIGHT {
                idle.push(TcpStream::connect(&authority).await.expect("connects"));
            }
            // One more: the listener closes it without reading or answering, even though it
            // carries this ceremony's redirect.
            let mut extra = TcpStream::connect(&authority).await.expect("connects");
            let _ = extra
                .write_all(
                    format!("GET /callback?state={STATE}&code=refused HTTP/1.1\r\n\r\n").as_bytes(),
                )
                .await;
            let mut answer = Vec::new();
            let answered = extra.read_to_end(&mut answer).await.map(|_| answer);
            // Freeing the slots (the listener reads EOF and answers 400) lets a redirect in.
            for mut connection in idle {
                connection.shutdown().await.expect("the write half closes");
                let mut response = String::new();
                connection
                    .read_to_string(&mut response)
                    .await
                    .expect("the response is read");
                assert!(response.starts_with("HTTP/1.1 400"), "{response}");
            }
            let real = get(
                &redirect_uri,
                &format!("/callback?state={STATE}&code=granted"),
            )
            .await;
            (answered, real)
        };
        let (redirect, (answered, real)) = tokio::time::timeout(
            Duration::from_secs(5),
            futures::future::join(listener.receive(STATE, &pages()), browser),
        )
        .await
        .expect("the listener recovers once the slots free");
        // Closed unanswered: either a clean EOF with no bytes, or a reset because the request
        // was never read.
        match answered {
            Ok(bytes) => assert!(bytes.is_empty(), "{}", String::from_utf8_lossy(&bytes)),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionReset, "{error}"),
        }
        assert!(real.starts_with("HTTP/1.1 200"), "{real}");
        assert_eq!(
            redirect,
            Redirect::Code {
                state: STATE.to_owned(),
                code: "granted".to_owned(),
            }
        );
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

    // ---- the device sign-in (issue #635) -----------------------------------------------------

    /// One scripted device answer: `None` is the next poll's server error.
    type DeviceAnswer = Option<fn() -> DeviceStep>;

    /// A device ceremony the test scripts: it answers each poll from a queue and records when,
    /// on the paused clock, each poll arrived.
    struct ScriptedDevice {
        expires_in: u64,
        interval_secs: u64,
        answers: Mutex<std::collections::VecDeque<DeviceAnswer>>,
        polled_at: Mutex<Vec<Duration>>,
        started: tokio::time::Instant,
    }

    impl ScriptedDevice {
        fn new(expires_in: u64, answers: Vec<DeviceAnswer>) -> Self {
            Self {
                expires_in,
                interval_secs: 5,
                answers: Mutex::new(answers.into()),
                polled_at: Mutex::new(Vec::new()),
                started: tokio::time::Instant::now(),
            }
        }

        fn polled_at(&self) -> Vec<u64> {
            self.polled_at
                .lock()
                .expect("unpoisoned")
                .iter()
                .map(Duration::as_secs)
                .collect()
        }
    }

    impl DeviceCeremony for ScriptedDevice {
        async fn begin_device(&self) -> Result<OidcDeviceAuthorization, RemoteError> {
            let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
            Ok(OidcDeviceAuthorization {
                handle: SecretString::from("handle"),
                binding: SecretString::from("binding"),
                user_code: "WDJB-MJHT".to_owned(),
                verification_uri: "https://idp.test/device".to_owned(),
                verification_uri_complete: None,
                interval_secs: self.interval_secs,
                expires_by: now + self.expires_in,
            })
        }

        async fn poll_device(
            &self,
            pending: &OidcDeviceAuthorization,
        ) -> Result<DeviceStep, RemoteError> {
            assert_eq!(pending.handle.expose_secret(), "handle");
            self.polled_at
                .lock()
                .expect("unpoisoned")
                .push(self.started.elapsed());
            let answer = self
                .answers
                .lock()
                .expect("unpoisoned")
                .pop_front()
                .unwrap_or(Some(|| DeviceStep::Pending));
            match answer {
                Some(step) => Ok(step()),
                None => Err(RemoteError::Auth(
                    capsule_sdk::auth::AuthError::OidcDeviceDenied,
                )),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_sign_in_waits_an_interval_before_every_poll_and_slows_down_when_told() {
        let ceremony = ScriptedDevice::new(
            600,
            vec![
                Some(|| DeviceStep::Pending),
                Some(|| DeviceStep::SlowDown),
                Some(|| DeviceStep::Pending),
                Some(|| DeviceStep::Settled(LoginStep::Complete)),
            ],
        );
        let mut shown = None;
        let step = login_device(
            &ceremony,
            |pending, waited| shown = Some((pending.user_code.clone(), waited)),
            DEVICE_TIMEOUT,
        )
        .await
        .expect("the sign-in settles");
        assert!(matches!(step, LoginStep::Complete));
        assert_eq!(
            shown,
            Some(("WDJB-MJHT".to_owned(), Duration::from_secs(600))),
            "the code is shown with the ceremony's own deadline, nearer than the ceiling"
        );
        // 5 s before the first poll, 5 more, then 10 after the slow-down, and 10 again.
        assert_eq!(ceremony.polled_at(), vec![5, 10, 20, 30]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_sign_in_never_approved_stops_at_its_deadline() {
        let ceremony = ScriptedDevice::new(12, vec![]);
        let error = login_device(&ceremony, |_, _| {}, DEVICE_TIMEOUT)
            .await
            .expect_err("never approved");
        assert!(
            matches!(error, OidcLoginError::DeviceTimedOut { waited } if waited == Duration::from_secs(10)),
            "{error:?}"
        );
        // A third poll would land at 15 s, past the 12-second deadline, so it is never sent.
        assert_eq!(ceremony.polled_at(), vec![5, 10]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_device_poll_ends_the_sign_in_with_the_servers_answer() {
        let ceremony = ScriptedDevice::new(600, vec![Some(|| DeviceStep::Pending), None]);
        let error = login_device(&ceremony, |_, _| {}, DEVICE_TIMEOUT)
            .await
            .expect_err("declined");
        assert!(
            matches!(
                error,
                OidcLoginError::Remote(RemoteError::Auth(
                    capsule_sdk::auth::AuthError::OidcDeviceDenied
                ))
            ),
            "{error:?}"
        );
        assert_eq!(ceremony.polled_at(), vec![5, 10]);
    }
}
