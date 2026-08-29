//! [`AccountDirectory`] — the one question login asks about a person.
//!
//! # Why this is a port and not a function
//!
//! Answering "is this person who they say they are" needs the account row, the stored password
//! hash and the failed-attempt count, all of which live in Postgres. Nothing else in the three
//! ported auth operations touches a database, so this is the whole of the server's dependency
//! on one — and keeping it behind a trait is what lets the surface be tested without a
//! container, exactly as [`AuthStateStore`](crate::store::AuthStateStore) does for session
//! state.
//!
//! # Why the outcome is three values and not a hash
//!
//! The obvious shape returns the stored password hash and verifies above the port. It was
//! rejected: a password hash that crosses this boundary is a secret in a type that does not
//! know it is one, printable by any `Debug`, and it puts the verification algorithm — Argon2id
//! parameters included — in the routing layer, where a second call site can get it subtly
//! wrong. Here the credential never rises above the adapter, and what comes back is the
//! decision:
//!
//! - [`Authentication::Granted`] — the password matched.
//! - [`Authentication::Locked`] — the account exists and is refusing attempts.
//! - [`Authentication::Refused`] — no such account, **or** the wrong password.
//!
//! The last one is deliberately a single value. The Salvo tree already went to the trouble of
//! verifying a dummy hash for an unknown email so the two paths take the same time; collapsing
//! them into one outcome means no caller *can* tell them apart, and no future caller can
//! reintroduce an enumeration oracle by branching on something this port does not offer.
//!
//! # What is deliberately absent
//!
//! **No attempt counter.** [`Authentication::Locked`] is account state the adapter owns —
//! the Salvo lockout is a column on the user row, not a windowed counter — so this port
//! records nothing and windows nothing. Rate limiting *is* a counter, it has no port anywhere
//! in this crate, and slice `S-C32` owns it; see `routes::auth` for what that costs the ported
//! surface today.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::store::UserId;

/// The future every directory operation returns.
///
/// Spelled out rather than `async fn` in trait position for the same reason the state ports
/// are ([`crate::store`]): it keeps the trait dyn-compatible, so the application context can
/// hold an `Arc<dyn AccountDirectory>` without the whole server becoming generic over it.
pub type DirectoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DirectoryError>> + Send + 'a>>;

/// What the directory decided about a presented credential.
///
/// Not a `Result`: none of the three is an error. A refused password is a normal answer to a
/// normal question, and modelling it as a failure is what leads to a `?` that turns a
/// credential rejection into a 500.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    /// The credential matched this account.
    Granted(UserId),
    /// The account is refusing attempts after too many failures.
    ///
    /// Distinct from [`Self::Refused`] because it is the one refusal a *correct* password also
    /// receives, so the user needs telling — a client that showed "wrong password" here would
    /// send them round a loop that cannot succeed.
    Locked,
    /// No such account, or the wrong password. One value on purpose — see the module docs.
    Refused,
}

/// What can go wrong reaching the account directory.
///
/// Narrow, and an operator diagnostic rather than a user-facing surface: mapping a failure onto
/// a status and an `error.*` code belongs to the route that could not finish, which knows what
/// the caller was trying to do.
///
/// `#[non_exhaustive]` because the Postgres adapter this slice does not write may need a
/// variant, and a route matching on it must keep compiling when one lands.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DirectoryError {
    /// The directory could not be reached, or refused to answer. No decision was reached, which
    /// is the only thing a caller can act on — whether the backend was down or merely angry
    /// changes nothing about the response.
    #[error("the account directory is unavailable: {detail}")]
    Unavailable {
        /// The backend's own description of the failure, for the log line.
        detail: String,
    },
}

/// Who exists, and whether a presented password is theirs.
///
/// One operation. A directory that also listed accounts, created them or changed passwords
/// would be the grab-bag `S-C29` deleted, rebuilt one method at a time; registration and
/// password change are their own surfaces and will bring their own contracts.
pub trait AccountDirectory: fmt::Debug + Send + Sync {
    /// Decide whether `password` authenticates the account named by `email`.
    ///
    /// The adapter owns credential verification end to end: the lookup, the constant-time
    /// comparison, the timing-equalized miss, and the failed-attempt bookkeeping that makes
    /// [`Authentication::Locked`] eventually true. `password` is borrowed and must not be
    /// retained, logged, or included in any error this returns.
    fn authenticate<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication>;
}
