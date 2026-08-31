//! Cross-device add (`S-C7`) — the code, the channel, and the freshness gate.
//!
//! # Three surfaces, three different callers
//!
//! Device **A** is signed in and initiates. Device **B** has no account, no session and no key
//! material — it is a phone that has just scanned a QR code. So the ceremony's operations do not
//! share an authentication model, and pretending they do would break it in one direction or the
//! other:
//!
//! | Operation | Caller | Gate |
//! | --- | --- | --- |
//! | issue a code | A | a session **plus** recent credential presentation |
//! | redeem a code | B | the code itself, which is the only thing B has |
//! | relay a payload | either | possession of the channel handle |
//! | close the channel | A | the session it was opened under |
//!
//! # The relay is a dumb pipe, and the design says so
//!
//! Anyone holding the channel handle may post in either direction. That is not an oversight:
//! design/device-enrollment.md puts channel integrity on the **safety-code check** — a short
//! code derived from the channel transcript, displayed on both devices beside each device's
//! identity — precisely because the relay is not trusted. *"Channel integrity never rests on the
//! code"*, and a relay that swapped in a different device is the attack the safety code catches.
//! So this server stores opaque payloads, delivers them once, and asserts nothing about who
//! wrote them.
//!
//! # What the freshness gate can and cannot mean
//!
//! Step 1 requires a **fresh local device authorization** on A — biometric or device passcode —
//! and *"a valid session token alone is not sufficient"*, so a stolen token cannot enroll a
//! rogue device.
//!
//! A server cannot verify a biometric. What it can verify is that the account holder proved a
//! credential recently, and that is what [`FRESH_AUTH_WINDOW`] enforces against
//! [`SessionRecord::authenticated_at`](crate::store::SessionRecord::authenticated_at). The
//! biometric half stays the client's, unverifiable and asserted — stated plainly rather than
//! implied, because a gate whose strength is misunderstood is worse than one that is absent.
//!
//! **`created_at` could not have carried this.** A refresh rotates the session and stamps a new
//! `created_at` every fifteen minutes, so a gate reading it would be satisfied by an attacker
//! doing nothing but refreshing. `authenticated_at` is set at sign-in and carried forward
//! untouched by a refresh, which is the whole reason it is a separate field.
//!
//! # Rate limiting is absent, and absent on purpose
//!
//! The contract rate-limits redemption, and the catalog carries `error.enrollment.rate_limited`
//! for it. The per-user counter that would enforce it has no port (`S-C32`), so the status is
//! **not declared** rather than declared and unreachable — the `S-C28` rule. What stands in
//! meanwhile is not nothing: a code is single-use, lives ten minutes, and carries ≥64 bits of
//! entropy, so the brute-force window is bounded by the TTL rather than by a counter.

use std::sync::Arc;

use jiff::SignedDuration;

use crate::store::{ChannelStore, EnrollmentStore};

/// How recently the account holder must have proved a credential to start a cross-device add.
///
/// Five minutes: long enough that signing in and immediately choosing "add a device" works
/// without a second prompt, short enough that a session left open on an unattended desk is not
/// a standing enrollment capability. The number is a deployment's to tune and the *shape* is
/// not: this is a window, never a flag a client can set.
pub const FRESH_AUTH_WINDOW: SignedDuration = SignedDuration::from_mins(5);

/// The largest relay payload this server will carry.
///
/// A wrapped master key plus a key bundle is a few kilobytes. The bound exists because the relay
/// is an *unauthenticated* write surface reachable by anyone holding a channel handle, and an
/// unbounded one would be a memory sink with a ten-minute TTL.
pub const MAX_RELAY_BYTES: usize = 64 * 1024;

/// The enrollment module's collaborators.
#[derive(Debug, Clone)]
pub struct EnrollmentContext {
    enrollments: Arc<dyn EnrollmentStore>,
    channels: Arc<dyn ChannelStore>,
    clock: Arc<dyn crate::store::Clock>,
    fresh_auth_window: SignedDuration,
}

impl EnrollmentContext {
    /// Assembles the module with the default freshness window.
    pub fn new(
        enrollments: Arc<dyn EnrollmentStore>,
        channels: Arc<dyn ChannelStore>,
        clock: Arc<dyn crate::store::Clock>,
    ) -> Self {
        Self {
            enrollments,
            channels,
            clock,
            fresh_auth_window: FRESH_AUTH_WINDOW,
        }
    }

    /// The same, with a deployment's own window.
    #[must_use]
    pub fn with_fresh_auth_window(mut self, window: SignedDuration) -> Self {
        self.fresh_auth_window = window;
        self
    }

    /// The pending enrollment codes.
    pub fn enrollments(&self) -> &dyn EnrollmentStore {
        self.enrollments.as_ref()
    }

    /// The relay channels.
    pub fn channels(&self) -> &dyn ChannelStore {
        self.channels.as_ref()
    }

    /// The clock the ceremony is timed by.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }

    /// How recently a credential must have been proved.
    pub fn fresh_auth_window(&self) -> SignedDuration {
        self.fresh_auth_window
    }

    /// Whether a session that last authenticated at `authenticated_at` may start an add.
    ///
    /// A pure comparison, so the gate is testable without a store and the same function decides
    /// it everywhere. Inclusive at the boundary: the window is a permission, not a margin to be
    /// conservative inside.
    pub fn is_fresh(&self, authenticated_at: jiff::Timestamp, now: jiff::Timestamp) -> bool {
        now.duration_since(authenticated_at) <= self.fresh_auth_window
    }
}

#[cfg(test)]
mod tests;
