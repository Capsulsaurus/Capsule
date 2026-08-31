//! The budgets this deployment enforces, in one place.
//!
//! Every limit the server applies is declared here rather than at its call site. That is not
//! tidiness: a budget written inline is a budget nobody can review against the threat model, and
//! two surfaces that ought to share a limit — the share path's two limiters and the drop path's
//! two, which the contracts explicitly call *the same two limiters* — would drift apart the
//! first time one was tuned.
//!
//! The numbers are deliberately conservative and deliberately adjustable. What is **not**
//! adjustable is where they live.

use jiff::SignedDuration;

use super::Budget;

/// Failed sign-ins per account before the account is locked out for a window.
///
/// Five in fifteen minutes. Low, because the thing being guessed is a password and the cost of a
/// legitimate user waiting is a support conversation while the cost of getting it wrong is an
/// account.
pub const LOGIN_ATTEMPTS: Budget = Budget::new(5, SignedDuration::from_mins(15));

/// Redemption attempts against one pending enrollment.
///
/// Ten in the code's own ten-minute lifetime, so the transcribable fallback — deliberately
/// shorter than the QR payload — cannot be ground through inside the window it exists in. This
/// is the limiter design/device-enrollment.md names as the reason the short form is safe to
/// offer at all.
pub const ENROLLMENT_REDEMPTION: Budget = Budget::new(10, SignedDuration::from_mins(10));

/// Requests against one share link's opaque id.
///
/// Sixty a minute: generous for a person opening a shared album, and a hard ceiling on how fast
/// one link can be probed. Enumeration across *many* ids is bounded by
/// [`SHARE_SOURCE`] and, structurally, by the 128-bit id itself.
pub const SHARE_LINK: Budget = Budget::new(60, SignedDuration::from_mins(1));

/// Requests from one source address on the public share path.
///
/// A hundred and twenty a minute. Higher than the per-link budget because one household behind
/// one address legitimately opens several shares; low enough that walking the id space from one
/// address is hopeless long before the entropy is.
pub const SHARE_SOURCE: Budget = Budget::new(120, SignedDuration::from_mins(1));

/// Drop-session creations against one upload link (invariant 31).
///
/// Thirty an hour. A guest depositing a holiday's photos makes tens of requests; a script
/// filling somebody's quota makes thousands.
pub const DROP_LINK: Budget = Budget::new(30, SignedDuration::from_hours(1));

/// Drop-session creations from one source address (invariant 31).
pub const DROP_SOURCE: Budget = Budget::new(60, SignedDuration::from_hours(1));

/// Deep storage verifications per account.
///
/// Four an hour. The contract calls the limiter *half of the feature*: a deep verify reads and
/// re-hashes every declared blob, so an unbounded one is an I/O-amplification attack costing the
/// attacker one small JSON body.
pub const DEEP_VERIFY: Budget = Budget::new(4, SignedDuration::from_hours(1));
