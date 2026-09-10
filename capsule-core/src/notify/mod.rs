//! Alert classes and their trigger predicates — the **one shared decision function** every
//! platform evaluates instead of reimplementing the taxonomy (slice `S-D29`, core half; SSoT:
//! [Notifications]).
//!
//! # The surface
//!
//! [`evaluate()`] turns a snapshot of device-held state ([`NotifyInput`]) into the [`Alert`]s
//! that are true at an instant. [`pre_arm_deadlines()`] returns the instant an OS timer must be
//! armed for, **per class** — a class absent from the map has no timer to hold.
//! [`next_deadline()`] is its minimum, for a caller that holds only one timer.
//!
//! Both are **pure**: no clock read, no socket, no SQLite, no `unsafe`, and no allocation beyond
//! the returned vector. `now` is always an argument, so the whole surface is driven by a mocked
//! clock in tests with no sleeps and no I/O — the same discipline as the recovery cadence whose
//! projection it consumes ([`RecoveryFacts`]).
//!
//! # Why every input is caller-supplied
//!
//! Nothing in this crate holds the trigger state. There is no `last_completed_sync` column, no
//! client-side quota type (server-held, and only as current as the last `GET /v1/quota`), and no
//! persisted quarantine table — a refused sync entry is a per-entry verdict
//! ([`crate::lifecycle::SyncApplyOutcome`]), not a row. Pending drops live in the provisioning
//! user's server-side inbox. So the predicate cannot read its own inputs; it takes them, which
//! is also what keeps it pure.
//!
//! [`NotifyInput`] therefore carries **counts and instants only** — no album ids, no titles, no
//! asset ids, nothing a server could author. That is forced rather than chosen: alert text is
//! composed on-device from decrypted state, and a key-free server has no plaintext to compose
//! from.
//!
//! # Delivery is not here
//!
//! This module decides *which classes are true* and *when to arm*. Presentation — a scheduled
//! `UNCalendarNotificationTrigger`, a `NotificationManagerCompat` post, an in-app badge — is
//! native per client, as is the permission prompt (asked the first time a class has something to
//! say, never at launch). The `notification.*` catalog keys land with the implementing client
//! slice, because the i18n guard requires a live consumer; this module emits a class and its
//! parameters, never a string a user reads.
//!
//! # Pre-arming, and what it costs
//!
//! An alert whose trigger is a deadline the device can compute MUST be pre-armed at the moment
//! that deadline becomes known, not evaluated when it expires — otherwise the staleness alert is
//! starved by the very absence of background windows it exists to report.
//!
//! The consequence for this module is the reason [`pre_arm_deadlines()`] is narrower than
//! [`evaluate()`]: an armed OS notification fires **without the app running**, so it cannot be
//! re-checked at fire time. An instant is therefore only returned when the alert is certain to be
//! true when it arrives — see [`AlertClass::pre_armable`] for which classes can be armed at all,
//! and [`pre_arm_deadlines()`] for the three conditions that withhold one from a pre-armable
//! class.
//!
//! Because the answer is a pure function of state, "re-arm on every state change that moves the
//! deadline" reduces on the client to: recompute after any state change, then reconcile the
//! timers against the map. One entry per class from one function is also why two live timers for
//! one class is structurally impossible.
//!
//! # Determinism
//!
//! [`Alert::params`] is a [`BTreeMap`](std::collections::BTreeMap), and [`evaluate()`] emits in
//! [`AlertClass::ALL`] order. Two calls on equal input are equal, byte-for-byte, through
//! `serde`.
//!
//! [Notifications]: https://docs/design/notifications/

pub(crate) mod class;
pub(crate) mod evaluate;
pub(crate) mod input;

pub use class::{Alert, AlertClass, AlertSeverity};
pub use evaluate::{DAY_SECS, SYNC_STALE_SECS, evaluate, next_deadline, pre_arm_deadlines};
pub use input::{NotifyInput, QuotaAdvisory, QuotaFacts, RecoveryFacts, SyncFacts};
