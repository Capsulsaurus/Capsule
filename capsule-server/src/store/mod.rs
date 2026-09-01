//! The server's state ports: two typed stores, four typed ceremony stores, and the adapters
//! behind them (slice `S-C29`).
//!
//! # What this module replaces
//!
//! The Salvo server kept all of this behind one trait, `SessionStorage`. That trait was not a
//! port, it was a grab-bag: session records, the per-user session index, MFA attempt counters
//! and rate-limit counters, plus `save_temp_data<T>` / `get_temp_data<T>` / `delete_temp_data`
//! — a generic serialize-anything key-value store with a **caller-supplied TTL**. Four
//! unrelated ceremonies rode it, namespaced only by hand-formatted string keys. That generic
//! blob store is the abstraction the Rust Architecture Decisions refuse, and this module
//! deletes it rather than porting it.
//!
//! # The three properties the shape enforces
//!
//! 1. **No operation takes an arbitrary serializable payload.** Every method names a concrete
//!    record type declared next to the trait that stores it. There is no `T: Serialize`
//!    anywhere in this module, and the record types deliberately derive no `serde` traits:
//!    the persistence encoding belongs to each *adapter*, not to the record, so a record
//!    cannot be smuggled through a store that was not built for it.
//! 2. **TTL is a property of the store, not an argument.** No method takes a lifetime. A
//!    ceremony's window belongs to what the ceremony *is*; it is fixed when the store is
//!    constructed and readable through `ttl()` for the routes that must publish an expiry.
//! 3. **A record and its index entry are one fact.** See [`auth`] — the port exposes no
//!    operation that names an index, so the split that produced the revoke-all over-count is
//!    not expressible.
//!
//! # Async shape
//!
//! The traits are written with explicit boxed futures ([`StoreFuture`]) rather than `async fn`
//! in trait position. That is deliberate and costs one `Box::pin` per call: it keeps every
//! port **dyn-compatible**, so application state can hold `Arc<dyn AuthStateStore>` and swap a
//! deterministic double for Valkey without making the whole server generic over its storage.
//! It is exactly what `async-trait` expands to, written out so the crate takes no dependency
//! for it.
//!
//! # Adapters, and what they are for
//!
//! Three adapters are planned per port — Postgres, Valkey, and a deterministic in-memory one —
//! and **three adapters are not three deployment modes**. Valkey is required; the server
//! refuses to boot without `VALKEY_URL` (design/filesystem/server.md, "Required Services").
//! The in-memory adapter in [`memory`] is a **test double**, never a deployment profile. The
//! rejected alternative was a Postgres fallback removing Valkey, which would mean emulating
//! TTL and expiry in SQL — the generic TTL abstraction this slice exists to delete.
//!
//! Whichever adapter is in play, it must pass the one shared suite in [`conformance`]. That
//! suite is what makes "the in-memory adapter behaves like Valkey" an assertion rather than an
//! assumption, and it is what lets the rest of the rebuild be tested without a container.
//!
//! # Not in this module
//!
//! MFA attempt counters and rate-limit counters also rode the Salvo grab-bag. They are
//! **counters, not records** — a different contract with a different failure mode (they must
//! survive a lost increment badly, not a lost record) — and `S-C29` assigns them no home.
//! They are deliberately absent here rather than folded into [`AuthStateStore`], where they
//! would rebuild the grab-bag one field at a time.

pub mod auth;
pub mod ceremony;
pub mod conformance;
pub mod ids;
pub mod memory;
pub mod upload;

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use jiff::{SignedDuration, Timestamp};

pub use self::auth::{AuthStateStore, CohortRecord, CohortStore, SessionRecord};
pub use self::ceremony::{
    CHALLENGE_TTL, ChallengeStore, ChannelStore, Direction, DrainOutcome, ENROLLMENT_CODE_TTL,
    EnrollmentStore, PendingEnrollment, RELAY_CHANNEL_TTL, RelayChannel, RelayOutcome,
    RelayPayload, RevokeAllChallenge,
};
pub use self::ids::{
    AlbumId, AssetId, ChallengeToken, ChannelId, EnrollmentCode, OwnerId, SessionId, UploadId,
    UserId,
};
pub use self::upload::{
    AcceptedChunk, BlobRole, FinalizeClaim, UploadSessionRecord, UploadSessionStatus,
    UploadSessionStore,
};

/// The future every port operation returns.
///
/// Spelled out rather than written as `async fn` in trait position so the traits stay
/// dyn-compatible — see the module docs. `'a` borrows both the store and the operation's
/// arguments, so a call site reads as an ordinary `store.op(&id).await`.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

/// What can go wrong reaching a state store.
///
/// Deliberately narrow, and deliberately **not** a user-facing surface: these are operator
/// diagnostics that reach the logs. Mapping a failed store operation onto an `error.*` code
/// and a localized message belongs to the route that could not complete, which knows what the
/// caller was trying to do; a store does not.
///
/// `#[non_exhaustive]` because an adapter this slice has not written yet may need a variant,
/// and a route matching on it must keep compiling when one lands.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The backend could not be reached at all — no connection, pool exhausted, timed out.
    /// The operation certainly did not happen.
    #[error("the {store} store is unavailable: {detail}")]
    Unavailable {
        /// Which port failed, for the log line.
        store: &'static str,
        /// The backend's own description of the failure.
        detail: String,
    },

    /// The backend was reached and refused the operation. Whether state changed is unknown,
    /// which is why this is distinct from [`StoreError::Unavailable`].
    #[error("the {store} store rejected the operation: {detail}")]
    Rejected {
        /// Which port failed.
        store: &'static str,
        /// The backend's own description of the refusal.
        detail: String,
    },

    /// A stored value could not be read back as the record type that owns its key. This is
    /// the failure the generic blob store made *routine* — a `get_temp_data::<A>` against a
    /// key written as `B` — and is retained only because a rolling deploy can genuinely leave
    /// an older encoding in the store.
    #[error("the {store} store holds a {record} value it cannot decode: {detail}")]
    Corrupt {
        /// Which port failed.
        store: &'static str,
        /// The record type the key belongs to.
        record: &'static str,
        /// What went wrong decoding it.
        detail: String,
    },
}

/// `at` advanced by `span`, clamped to the end of representable time.
///
/// [`Timestamp::checked_add`] is fallible because a *calendar* span can be ambiguous; a
/// [`SignedDuration`] never is, so the only failure reachable here is arithmetic overflow past
/// [`Timestamp::MAX`] — and a deadline that far out is indistinguishable from never expiring,
/// which is exactly what clamping gives.
pub(crate) fn deadline(at: Timestamp, span: SignedDuration) -> Timestamp {
    at.checked_add(span).unwrap_or(Timestamp::MAX)
}

/// The clock a store reads to decide what has expired.
///
/// Injected rather than called directly so expiry is testable without sleeping: the
/// deterministic double in [`memory::ManualClock`] advances on demand, which is what lets the
/// [`conformance`] suite assert TTL behaviour in a unit test.
pub trait Clock: fmt::Debug + Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// The clock production adapters read.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}
