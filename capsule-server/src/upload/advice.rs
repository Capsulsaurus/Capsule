//! Server advice and a session's relative expiry — what session create and `HEAD` tell a
//! client beside the session itself.
//!
//! # Advice is tunable, not protocol
//!
//! `X-Capsule-Advice` is an [RFC 9651] structured-field dictionary of integers (networking
//! design doc, Server Advice): `chunk` (suggested chunk bytes), `sessions` (in-flight sessions
//! per lane), `ranges` (in-flight ranges per session) and `ttl` (seconds the advice holds). The
//! client clamps every member into bounds compiled into it and ignores members it does not
//! know, so a value here can only steer a client inside limits it already enforces; nothing in
//! this module is a promise the server keeps. It replaces the single-valued
//! `X-Capsule-Suggested-Chunk-Size`, whose number is the `chunk` member.
//!
//! # Expiry travels beside advice, relative
//!
//! A session's own lifetime is not advice: it is a fact about that session. Create and `HEAD`
//! state it as seconds remaining (`X-Capsule-Session-Expires-In` to the lifetime cap,
//! `X-Capsule-Session-Idle-Expires-In` to the survival floor), never as an instant, so a client
//! whose clock is skewed still schedules correctly (transfer upload design doc, Session
//! Lifetime and Discard).
//!
//! [RFC 9651]: https://www.rfc-editor.org/rfc/rfc9651

use std::fmt;

use jiff::{SignedDuration, Timestamp};

use crate::store::UploadSessionRecord;
use crate::upload::chunk::suggested_chunk_size;

/// In-flight sessions per lane the server advises.
///
/// A lane's window is client-local and this can only lower it; four keeps a high-latency link
/// busy without letting one device hold a large share of the live-session cap.
pub const ADVISED_SESSIONS: u64 = 4;

/// In-flight ranges per session the server advises.
///
/// One: this server appends a session's bytes in order — a chunk at any offset but the
/// committed one is `409 error.upload.offset_mismatch` — so a second range in flight could
/// only be refused. The member is fixed for a session's life, which is what lets it rise once
/// the server accepts ranges out of order.
pub const ADVISED_RANGES: u64 = 1;

/// How long, in seconds, a client may hold the advice before it reverts to its own defaults.
pub const ADVICE_TTL_SECS: u64 = 300;

/// The survival floor: a `Pending` or `Uploading` session survives at least this long after
/// its last accepted chunk, or its creation when it has none.
///
/// The lifetime cap above it is the store's own TTL ([`crate::store::upload::LIFETIME_CAP`]).
pub const SURVIVAL_FLOOR: SignedDuration = SignedDuration::from_hours(1);

/// What `X-Capsule-Advice` carries for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advice {
    /// Suggested chunk bytes.
    pub chunk: u64,
    /// In-flight sessions per lane.
    pub sessions: u64,
    /// In-flight ranges per session.
    pub ranges: u64,
    /// Seconds the advice holds.
    pub ttl: u64,
}

impl Advice {
    /// The advice for a session declaring `total_size` bytes.
    pub fn for_size(total_size: u64) -> Self {
        Self {
            chunk: suggested_chunk_size(total_size),
            sessions: ADVISED_SESSIONS,
            ranges: ADVISED_RANGES,
            ttl: ADVICE_TTL_SECS,
        }
    }
}

/// The RFC 9651 dictionary serialization: `chunk=262144, sessions=4, ranges=1, ttl=300`.
///
/// Every member is a bare integer, which the dictionary grammar writes as `key=digits` with no
/// parameters; members are separated by a comma and one space, the canonical form.
impl fmt::Display for Advice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "chunk={}, sessions={}, ranges={}, ttl={}",
            self.chunk, self.sessions, self.ranges, self.ttl
        )
    }
}

/// A session's remaining lifetime, in whole seconds from `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionExpiry {
    /// Seconds until the lifetime cap (`X-Capsule-Session-Expires-In`).
    pub expires_in: u64,
    /// Seconds until the survival floor (`X-Capsule-Session-Idle-Expires-In`).
    ///
    /// Never more than [`Self::expires_in`]: the cap ends a session whatever its progress. For a
    /// session past `Uploading` — finalizing or terminal — it equals the cap's, because such a
    /// session is never discarded under pressure and lives to the cap.
    pub idle_expires_in: u64,
}

impl SessionExpiry {
    /// `record`'s expiry under a store whose lifetime cap is `cap`, as of `now`.
    ///
    /// Both values floor at zero: a session the store has not yet swept is reported as due now
    /// rather than with a negative count a client would have to special-case.
    pub fn of(record: &UploadSessionRecord, cap: SignedDuration, now: Timestamp) -> Self {
        let expires_in = seconds_until(record.created_at, cap, now);
        let idle_expires_in = if record.status.is_evictable() {
            seconds_until(record.last_progress_at, SURVIVAL_FLOOR, now).min(expires_in)
        } else {
            expires_in
        };
        Self {
            expires_in,
            idle_expires_in,
        }
    }
}

/// Whole seconds from `now` to `from + span`, or zero once that instant has passed.
fn seconds_until(from: Timestamp, span: SignedDuration, now: Timestamp) -> u64 {
    let deadline = from.as_second().saturating_add(span.as_secs());
    u64::try_from(deadline.saturating_sub(now.as_second())).unwrap_or(0)
}

#[cfg(test)]
mod tests;
