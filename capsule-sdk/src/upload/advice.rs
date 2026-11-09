//! Server advice and a session's relative expiry, read leniently.
//!
//! The server tunes clients it cannot redeploy through one `X-Capsule-Advice` header on session
//! create and `HEAD`: an [RFC 9651] structured-field dictionary of integers whose members are
//! `chunk` (suggested chunk bytes), `sessions` (in-flight sessions per lane), `ranges`
//! (in-flight ranges per session) and `ttl` (seconds the advice holds) — networking design doc,
//! Server Advice. Advice is never an instruction, so reading it never fails:
//!
//! - **Clamped.** Every member is clamped into bounds compiled into this client. A window member
//!   can only lower the client's own cap: the effective window is the smaller of the two.
//! - **Lenient.** A member this client does not know is ignored, a member whose value is not an
//!   integer is treated as absent, and an absent member — or an absent or malformed header — is
//!   the client's own default.
//! - **Bounded in time.** Advice older than its `ttl` is dropped, except `ranges`, which is fixed
//!   for the session's life.
//!
//! The session's own expiry travels beside the advice as seconds remaining
//! (`X-Capsule-Session-Expires-In`, `X-Capsule-Session-Idle-Expires-In`), never as an instant,
//! so a skewed device clock still schedules correctly ([`SessionExpiry`]).
//!
//! [RFC 9651]: https://www.rfc-editor.org/rfc/rfc9651

use std::time::Duration;

use super::{ALIGNMENT, PROTOCOL_MAX_CHUNK, PROTOCOL_MIN_CHUNK};

/// The advice header.
pub const ADVICE_HEADER: &str = "X-Capsule-Advice";
/// Seconds until the session's lifetime cap.
pub const EXPIRES_IN_HEADER: &str = "X-Capsule-Session-Expires-In";
/// Seconds until the session's survival floor.
pub const IDLE_EXPIRES_IN_HEADER: &str = "X-Capsule-Session-Idle-Expires-In";

/// The most in-flight sessions per lane any advice can grant.
pub const MAX_ADVISED_SESSIONS: u32 = 16;
/// The most in-flight ranges per session any advice can grant: the protocol's ceiling on a
/// session's in-flight cap (transfer upload design doc, Bounded fan-in).
pub const MAX_ADVISED_RANGES: u32 = 16;
/// How long advice that names no usable `ttl` holds.
pub const DEFAULT_ADVICE_TTL: Duration = Duration::from_secs(300);
/// The longest any advice holds, whatever its `ttl` says.
pub const MAX_ADVICE_TTL: Duration = Duration::from_secs(3600);
/// A header longer than this is not read at all: advice is a handful of integers, and a header
/// past this length is a server bug or a hostile one, either way the client's defaults.
pub const MAX_ADVICE_LEN: usize = 1024;

/// What one `X-Capsule-Advice` header said, already clamped into this client's bounds.
///
/// Every member is `None` where the header did not carry a usable value; the accessors then
/// answer the caller's own default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerAdvice {
    chunk: Option<u64>,
    sessions: Option<u32>,
    ranges: Option<u32>,
    ttl: Duration,
}

impl Default for ServerAdvice {
    fn default() -> Self {
        Self::absent()
    }
}

impl ServerAdvice {
    /// No advice: every accessor answers the caller's default.
    pub const fn absent() -> Self {
        Self {
            chunk: None,
            sessions: None,
            ranges: None,
            ttl: Duration::ZERO,
        }
    }

    /// Read a header value, which may be missing.
    ///
    /// Never fails: a missing, oversized or malformed header is [`Self::absent`], an unknown
    /// member is skipped, and a known member whose value is not an integer is treated as absent.
    /// Where a member repeats, the last one wins, as RFC 9651 dictionaries specify.
    pub fn parse(header: Option<&str>) -> Self {
        let Some(header) = header else {
            return Self::absent();
        };
        if header.len() > MAX_ADVICE_LEN {
            tracing::debug!(len = header.len(), "ignoring an oversized advice header");
            return Self::absent();
        }
        let mut chunk = None;
        let mut sessions = None;
        let mut ranges = None;
        let mut ttl = None;
        for member in members(header) {
            let Some((key, value)) = integer_member(member) else {
                continue;
            };
            match key {
                "chunk" => chunk = Some(clamp_chunk(value)),
                "sessions" => sessions = Some(clamp_window(value, MAX_ADVISED_SESSIONS)),
                "ranges" => ranges = Some(clamp_window(value, MAX_ADVISED_RANGES)),
                "ttl" => ttl = Some(clamp_ttl(value)),
                // Additive: a member a newer server sends is one this client ignores.
                _ => {}
            }
        }
        let advice = Self {
            chunk,
            sessions,
            ranges,
            ttl: ttl.unwrap_or(DEFAULT_ADVICE_TTL),
        };
        tracing::trace!(?advice, "read server advice");
        advice
    }

    /// Whether advice received `age` ago still holds.
    pub fn is_fresh(&self, age: Duration) -> bool {
        age < self.ttl
    }

    /// How long the advice holds from receipt.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The suggested chunk size, 4 KiB-aligned and within the protocol bounds, or `None` when
    /// the advice named none or is older than its `ttl`.
    pub fn chunk(&self, age: Duration) -> Option<u64> {
        self.chunk.filter(|_| self.is_fresh(age))
    }

    /// The in-flight sessions a lane may hold: the smaller of `own` and the advice, or `own`
    /// when the advice named none or is older than its `ttl`. Never below one.
    pub fn sessions_window(&self, own: u32, age: Duration) -> u32 {
        let own = own.max(1);
        match self.sessions {
            Some(advised) if self.is_fresh(age) => own.min(advised),
            _ => own,
        }
    }

    /// The in-flight ranges a session may hold: the smaller of `own` and the advice, or `own`
    /// when the advice named none. Never below one.
    ///
    /// Takes no age: the server fixes a session's in-flight cap at creation, so the `ranges` a
    /// session was advised holds for that session's whole life.
    pub fn ranges_window(&self, own: u32) -> u32 {
        let own = own.max(1);
        self.ranges.map_or(own, |advised| own.min(advised))
    }
}

/// A session's remaining lifetime as the server stated it, relative to the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionExpiry {
    /// Time left until the session's lifetime cap, when the server said.
    pub expires_in: Option<Duration>,
    /// Time left until the session's survival floor — after which it may be discarded under
    /// pressure — when the server said. Never more than [`Self::expires_in`] once read.
    pub idle_expires_in: Option<Duration>,
}

impl SessionExpiry {
    /// Read the two header values, either of which may be missing or malformed.
    ///
    /// A value that is not a non-negative integer number of seconds is absent. An idle expiry
    /// past the lifetime cap is cut back to it, because the cap ends a session whatever its
    /// progress.
    pub fn parse(expires_in: Option<&str>, idle_expires_in: Option<&str>) -> Self {
        let seconds = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|text| text.parse::<u64>().ok())
                .map(Duration::from_secs)
        };
        let expires_in = seconds(expires_in);
        let idle_expires_in = match (seconds(idle_expires_in), expires_in) {
            (Some(idle), Some(cap)) => Some(idle.min(cap)),
            (idle, _) => idle,
        };
        Self {
            expires_in,
            idle_expires_in,
        }
    }
}

/// The dictionary's members, split at the commas that separate them.
///
/// A comma inside a quoted string or an inner list belongs to that value, so the split tracks
/// both rather than cutting at every comma: a hostile string value cannot smuggle in a member.
fn members(header: &str) -> impl Iterator<Item = &str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0_usize;
    for (index, byte) in header.bytes().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&header[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&header[start..]);
    parts.into_iter().map(str::trim)
}

/// A `key=integer` member, its parameters ignored; `None` for anything else.
fn integer_member(member: &str) -> Option<(&str, i64)> {
    let (key, rest) = member.split_once('=')?;
    if !is_key(key) {
        return None;
    }
    let value = rest.split(';').next().unwrap_or_default();
    Some((key, parse_integer(value)?))
}

/// An RFC 9651 key: a lowercase letter or `*`, then lowercase letters, digits, `_`, `-`, `.`
/// or `*`.
fn is_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == b'*')
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-.*".contains(&byte)
        })
}

/// An RFC 9651 integer: an optional `-`, then one to fifteen digits, nothing else.
fn parse_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A chunk size aligned down to the 4 KiB grid and clamped into the protocol bounds.
fn clamp_chunk(value: i64) -> u64 {
    let value = u64::try_from(value).unwrap_or(0);
    ((value / ALIGNMENT) * ALIGNMENT).clamp(PROTOCOL_MIN_CHUNK, PROTOCOL_MAX_CHUNK)
}

/// A window clamped into `[1, ceiling]`.
fn clamp_window(value: i64, ceiling: u32) -> u32 {
    u32::try_from(value.clamp(1, i64::from(ceiling))).unwrap_or(1)
}

/// A time to live clamped into `[0, MAX_ADVICE_TTL]`.
fn clamp_ttl(value: i64) -> Duration {
    let seconds = u64::try_from(value).unwrap_or(0);
    Duration::from_secs(seconds).min(MAX_ADVICE_TTL)
}

#[cfg(test)]
mod tests;
