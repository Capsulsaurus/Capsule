//! The response half of the protocol handshake, read on every call (issue #674).
//!
//! # The contract
//!
//! [Versioning — Update Policy] has the server advertise its accepted `protocol_version`
//! window (`X-Capsule-Protocol-Min`/`-Max`) and an advisory client-build floor
//! (`X-Capsule-Min-Client-Build`) on every response, and has a native app refuse to *write* —
//! telling the user to update — when the server answers `426` or advertises a floor above its
//! own build. Reads keep working against any server: "reads of any past version succeed", and
//! "reads are never affected" by either rule. System invariant 8 makes the protocol date one
//! value per binary.
//!
//! # What this module does with that
//!
//! - [`ServerWindow`] is the three headers, parsed leniently: a header that is absent or does
//!   not parse is no bound, never a refusal (apps stay lenient).
//! - [`UpgradeRequired`] is the **one** typed refusal every SDK surface maps to: a `426` the
//!   server answered ([`UpgradeReason::Refused`]), or a window or floor this build is outside.
//! - The window each server advertised is latched per origin ([`observe`]) from every
//!   response the SDK's seams see — reads, writes and uploads alike — so a later write is
//!   refused **before** it leaves ([`preflight`]) instead of after the server acted on it.
//!   [`advertised`] and [`upgrade_required_for`] expose the latch, which is how a read that
//!   was served surfaces "this build is too old" to a UI without being refused.
//!
//! A write whose own response first reveals the floor has already landed; it is returned as
//! the server answered (it cannot be unsent, and reporting a landed write as failed invites
//! a duplicate), and the latch refuses the next one.
//!
//! [Versioning — Update Policy]: https://docs/design/versioning/#update-policy

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use capsule_i18n::error_codes;
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode, Url};

/// The request header carrying the protocol date a request is written against.
pub const PROTOCOL_HEADER: &str = "x-capsule-protocol";
/// The response header carrying the oldest protocol date the server accepts.
pub const PROTOCOL_MIN_HEADER: &str = "x-capsule-protocol-min";
/// The response header carrying the newest protocol date the server accepts.
pub const PROTOCOL_MAX_HEADER: &str = "x-capsule-protocol-max";
/// The response header carrying the server's advisory client-build floor (`MAJOR.MINOR.PATCH`).
pub const MIN_CLIENT_BUILD_HEADER: &str = "x-capsule-min-client-build";

/// The protocol date this binary speaks — `capsule-core`'s, and nothing else (invariant 8).
pub const PROTOCOL_VERSION: &str = capsule_core::crypto::primitives::PROTOCOL_VERSION;

/// The client build compared against `X-Capsule-Min-Client-Build`: this SDK's own version,
/// which every Capsule client on this tree ships as its network path.
pub const CLIENT_BUILD: &str = env!("CARGO_PKG_VERSION");

/// What a server advertised about the clients it accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerWindow {
    /// `X-Capsule-Protocol-Min`, verbatim.
    pub protocol_min: Option<String>,
    /// `X-Capsule-Protocol-Max`, verbatim.
    pub protocol_max: Option<String>,
    /// `X-Capsule-Min-Client-Build`, verbatim.
    pub min_client_build: Option<String>,
}

impl ServerWindow {
    /// Read the three advertised headers off a response.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let read = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Self {
            protocol_min: read(PROTOCOL_MIN_HEADER),
            protocol_max: read(PROTOCOL_MAX_HEADER),
            min_client_build: read(MIN_CLIENT_BUILD_HEADER),
        }
    }

    /// Whether the response advertised anything at all.
    #[must_use]
    pub fn is_advertised(&self) -> bool {
        self.protocol_min.is_some()
            || self.protocol_max.is_some()
            || self.min_client_build.is_some()
    }

    /// Why a client speaking `protocol` at `build` is outside this window, or `None` when it is
    /// inside (or when no bound that parses says otherwise).
    #[must_use]
    pub fn verdict(&self, protocol: &str, build: &str) -> Option<UpgradeReason> {
        if is_protocol_date(protocol) {
            if let Some(min) = self.protocol_min.as_deref().filter(|v| is_protocol_date(v))
                && protocol < min
            {
                return Some(UpgradeReason::ProtocolBelowWindow);
            }
            if let Some(max) = self.protocol_max.as_deref().filter(|v| is_protocol_date(v))
                && protocol > max
            {
                return Some(UpgradeReason::ProtocolAboveWindow);
            }
        }
        let floor = self.min_client_build.as_deref().and_then(parse_build);
        match (floor, parse_build(build)) {
            (Some(floor), Some(build)) if build < floor => Some(UpgradeReason::BuildBelowFloor),
            _ => None,
        }
    }
}

/// Why a server will not take this client's writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeReason {
    /// The server answered `426 Upgrade Required`.
    Refused,
    /// This client's protocol date is older than `X-Capsule-Protocol-Min`: update the app.
    ProtocolBelowWindow,
    /// This client's protocol date is newer than `X-Capsule-Protocol-Max`: the server is behind.
    ProtocolAboveWindow,
    /// This client's build is below `X-Capsule-Min-Client-Build`: update the app.
    BuildBelowFloor,
}

/// The one typed refusal for "this client and this server cannot write together", whichever
/// SDK surface met it. Carries the advertised window so a UI can say what to update.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "upgrade required ({reason:?}): server accepts protocol {min:?}..={max:?} and client build \
     >= {floor:?}; this client speaks {protocol} at build {build}: {message}",
    min = window.protocol_min,
    max = window.protocol_max,
    floor = window.min_client_build
)]
pub struct UpgradeRequired {
    /// Which rule refused.
    pub reason: UpgradeReason,
    /// What the server advertised.
    pub window: ServerWindow,
    /// The protocol date the refused request carried.
    pub protocol: String,
    /// The client build compared against the floor.
    pub build: String,
    /// The English detail: the server's, for a `426`; this module's, for a local refusal.
    pub message: String,
}

impl UpgradeRequired {
    /// The stable `error.*` catalog code a client localizes.
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        error_codes::PROTOCOL_VERSION_UNSUPPORTED
    }

    /// A `426` the server answered, carrying whatever window it advertised beside it.
    #[must_use]
    pub fn refused(headers: &HeaderMap, protocol: &str, message: impl Into<String>) -> Self {
        Self {
            reason: UpgradeReason::Refused,
            window: ServerWindow::from_headers(headers),
            protocol: protocol.to_owned(),
            build: CLIENT_BUILD.to_owned(),
            message: message.into(),
        }
    }
}

/// Whether a request with this method writes. Only a write is ever refused here.
#[must_use]
pub fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The protocol date a prepared request carries — its own header when it set one (the
/// upload transport's pin), else the build's, which the transport adds as a default header.
#[must_use]
pub fn protocol_of(headers: &HeaderMap) -> String {
    headers
        .get(PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or(PROTOCOL_VERSION)
        .to_owned()
}

/// Latch the window `url`'s server advertised on a response. A response that advertises
/// nothing (an unrouted `404`, a proxy's error page) leaves the latch as it was.
pub fn observe(url: &Url, headers: &HeaderMap) {
    let window = ServerWindow::from_headers(headers);
    if !window.is_advertised() {
        return;
    }
    let origin = origin_of(url);
    let mut latch = latch().lock().unwrap_or_else(PoisonError::into_inner);
    if latch.get(&origin) != Some(&window) {
        tracing::debug!(
            %origin,
            protocol_min = ?window.protocol_min,
            protocol_max = ?window.protocol_max,
            min_client_build = ?window.min_client_build,
            "server window observed"
        );
        latch.insert(origin, window);
    }
}

/// The window `url`'s server last advertised to this process, if any.
#[must_use]
pub fn advertised(url: &Url) -> Option<ServerWindow> {
    latch()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&origin_of(url))
        .cloned()
}

/// Whether this build is outside the window `url`'s server last advertised — the read-side
/// signal: a read is served regardless, and this is what tells the UI to ask for an update.
#[must_use]
pub fn upgrade_required_for(url: &Url) -> Option<UpgradeRequired> {
    let window = advertised(url)?;
    let reason = window.verdict(PROTOCOL_VERSION, CLIENT_BUILD)?;
    Some(local_refusal(reason, window, PROTOCOL_VERSION))
}

/// Refuse a write the latched window already says the server will not take, before it leaves.
/// Reads are never refused.
///
/// # Errors
///
/// [`UpgradeRequired`] when `method` writes and `protocol`/[`CLIENT_BUILD`] is outside the
/// window `url`'s server last advertised.
pub fn preflight(method: &Method, url: &Url, protocol: &str) -> Result<(), UpgradeRequired> {
    if !is_write(method) {
        return Ok(());
    }
    let Some(window) = advertised(url) else {
        return Ok(());
    };
    match window.verdict(protocol, CLIENT_BUILD) {
        None => Ok(()),
        Some(reason) => {
            let refusal = local_refusal(reason, window, protocol);
            tracing::warn!(%method, %url, ?reason, "write refused before sending: {refusal}");
            Err(refusal)
        }
    }
}

/// Latch a response's window and map a `426` to [`UpgradeRequired`]; every other response is
/// returned untouched for its caller to read.
///
/// # Errors
///
/// [`UpgradeRequired`] for a `426`, with the problem's `detail` (or the body as text) as its
/// message.
pub async fn screen(
    protocol: &str,
    response: reqwest::Response,
) -> Result<reqwest::Response, UpgradeRequired> {
    observe(response.url(), response.headers());
    if response.status() != StatusCode::UPGRADE_REQUIRED {
        return Ok(response);
    }
    let headers = response.headers().clone();
    let url = response.url().clone();
    let text = response.text().await.unwrap_or_default();
    let refusal = UpgradeRequired::refused(&headers, protocol, problem_detail(&text));
    tracing::warn!(%url, "server refused this client's protocol: {refusal}");
    Err(refusal)
}

/// The English detail of a problem body: `detail`, else the legacy `error`, else the text.
#[must_use]
pub fn problem_detail(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|body| {
            ["detail", "error", "title"]
                .iter()
                .find_map(|key| body.get(key)?.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| text.to_owned())
}

fn local_refusal(reason: UpgradeReason, window: ServerWindow, protocol: &str) -> UpgradeRequired {
    UpgradeRequired {
        reason,
        window,
        protocol: protocol.to_owned(),
        build: CLIENT_BUILD.to_owned(),
        message: "the server's advertised window does not admit this client's writes; \
                  update Capsule"
            .to_owned(),
    }
}

fn latch() -> &'static Mutex<HashMap<String, ServerWindow>> {
    static LATCH: OnceLock<Mutex<HashMap<String, ServerWindow>>> = OnceLock::new();
    LATCH.get_or_init(|| Mutex::new(HashMap::new()))
}

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// `YYYY-MM-DD`, the grammar in which lexical order is date order.
fn is_protocol_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// `MAJOR.MINOR.PATCH`, compared numerically. Anything after a `-` or `+` is ignored; a value
/// that is not three numbers is no floor.
fn parse_build(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let parsed = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(parsed)
}

#[cfg(test)]
mod tests;
