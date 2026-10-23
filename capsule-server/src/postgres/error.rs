//! One `DbErr` → [`StoreError`] mapping, for every Postgres adapter (#402).
//!
//! # Why one function and not one per adapter
//!
//! [`StoreError`]'s variants are a *decision* a caller acts on — the operation certainly did not
//! happen, the operation may or may not have happened, a uniqueness constraint already holds the
//! key, the stored value cannot be read back — and four adapters classifying the same `DbErr` four times is four chances to get that
//! decision subtly different. The route above them maps the variant onto an `error.*` code, so a
//! divergence here shows up as one port answering `503` where another answers `500` for the same
//! dropped socket.
//!
//! # How the variants are decided
//!
//! On the `DbErr` variant, plus the one SQLSTATE sea-orm itself already exposes through
//! [`DbErr::sql_err`], and without reaching into `sqlx::Error`. Two reasons, and the second is
//! the load-bearing one:
//!
//! - Naming `sqlx::Error` means depending on `sqlx` directly, which is a second pin of a crate
//!   sea-orm already owns.
//! - **A statement that failed mid-flight has not "certainly not happened".**
//!   [`StoreError::Unavailable`] promises exactly that, so the tempting mapping — any I/O error
//!   under `DbErr::Exec` is `Unavailable` — would be a lie in the one case it matters: a
//!   connection dropped after the server committed. That case is
//!   [`StoreError::Rejected`], whose contract is "whether state changed is unknown". So
//!   `Unavailable` is reserved for the failures that happen *before* a statement is sent —
//!   acquiring a connection, opening one — and everything else that is not a decoding failure is
//!   `Rejected`.
//!
//! The exception is a unique violation (SQLSTATE `23505`), which is
//! [`StoreError::Conflict`] (#523). It is the one failed-statement outcome whose effect *is*
//! known — Postgres refused the row, and the transaction it ran in is aborted — and reporting it
//! as `Rejected` made a key collision read as an internal error. `sql_err` is sea-orm's own
//! reading of the driver's code, so this still names no `sqlx` type.
//!
//! `DbErr::RecordNotFound` never reaches here as an error: every port in this crate models
//! absence as an `Option` or an outcome variant, and the adapters answer it from a row count
//! rather than from a raised error.

use sea_orm::{DbErr, SqlErr};

use crate::store::StoreError;

/// Which port is speaking, and what record it holds.
///
/// A value rather than two parameters at every call site: an adapter declares it once as a
/// constant and every `map_err` reads as the operation it was doing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Port {
    /// The port's name, for the log line and the `StoreError` field.
    pub(crate) store: &'static str,
    /// The record type its rows decode into.
    pub(crate) record: &'static str,
}

impl Port {
    /// A closure that turns a `DbErr` raised while `doing` into a [`StoreError`].
    ///
    /// Returned as a closure so a call site reads
    /// `.map_err(PORT.failing("reserving an asset row"))?`, with the operation named once beside
    /// the statement that performs it rather than repeated in a `match`.
    pub(crate) fn failing(self, doing: &'static str) -> impl Fn(DbErr) -> StoreError {
        move |error| self.classify(doing, &error)
    }

    /// A stored value that could not be read back as the record type that owns its key.
    ///
    /// Raised by the adapters themselves — an unknown enum discriminant, a content address that
    /// no longer parses, an `i64` that is not a valid instant — rather than by the driver. It is
    /// the variant `store/mod.rs` keeps for a rolling deploy that left an older encoding behind.
    pub(crate) fn undecodable(self, detail: impl Into<String>) -> StoreError {
        let detail = detail.into();
        tracing::error!(
            store = self.store,
            record = self.record,
            %detail,
            "a stored row could not be read back as the record that owns it"
        );
        StoreError::Corrupt {
            store: self.store,
            record: self.record,
            detail,
        }
    }

    /// The classification itself, split out so it is testable without a database.
    fn classify(self, doing: &str, error: &DbErr) -> StoreError {
        let detail = format!("{doing}: {error}");
        match error {
            // Before any statement was sent. The operation certainly did not happen.
            DbErr::Conn(_) | DbErr::ConnectionAcquire(_) => {
                tracing::error!(store = self.store, %detail, "the Postgres backend is unreachable");
                StoreError::Unavailable {
                    store: self.store,
                    detail,
                }
            }
            // The driver reached a value it could not turn into the Rust type the column is
            // read as — a schema the running binary was not built against.
            DbErr::Type(_)
            | DbErr::Json(_)
            | DbErr::TryIntoErr { .. }
            | DbErr::ConvertFromU64(_) => self.undecodable(detail),
            // The statement was sent and a unique index refused it. Nothing was written, and
            // the key is held — a conflict, not an internal failure. Logged at `warn` like every
            // other refusal: every port that expects a conflict decides it with `ON CONFLICT`, so
            // one that reaches here is an unexpected collision (a minted id hitting a key).
            _ if is_unique_violation(error) => {
                tracing::warn!(store = self.store, %detail, "a Postgres unique index refused a write");
                StoreError::Conflict {
                    store: self.store,
                    detail,
                }
            }
            // Everything else: the statement was sent and did not succeed. Whether state
            // changed is unknown, which is exactly what `Rejected` means.
            _ => {
                tracing::warn!(store = self.store, %detail, "Postgres refused an operation");
                StoreError::Rejected {
                    store: self.store,
                    detail,
                }
            }
        }
    }
}

/// Whether `error` is Postgres refusing a row a unique index already holds (SQLSTATE `23505`).
fn is_unique_violation(error: &DbErr) -> bool {
    matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

#[cfg(test)]
mod tests;
