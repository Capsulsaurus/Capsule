//! One `DbErr` → [`StoreError`] mapping, for every Postgres adapter (#402).
//!
//! # Why one function and not one per adapter
//!
//! [`StoreError`]'s three variants are a *decision* a caller acts on — the operation certainly
//! did not happen, the operation may or may not have happened, the stored value cannot be read
//! back — and four adapters classifying the same `DbErr` four times is four chances to get that
//! decision subtly different. The route above them maps the variant onto an `error.*` code, so a
//! divergence here shows up as one port answering `503` where another answers `500` for the same
//! dropped socket.
//!
//! # How the three variants are decided
//!
//! Deliberately on the `DbErr` variant alone, without reaching into `sqlx::Error`. Two reasons,
//! and the second is the load-bearing one:
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
//! `DbErr::RecordNotFound` never reaches here as an error: every port in this crate models
//! absence as an `Option` or an outcome variant, and the adapters answer it from a row count
//! rather than from a raised error.

use sea_orm::DbErr;

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

#[cfg(test)]
mod tests {
    use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};

    use super::Port;
    use crate::store::StoreError;

    const PORT: Port = Port {
        store: "test",
        record: "TestRecord",
    };

    #[test]
    fn a_pool_timeout_is_unavailable_because_nothing_was_sent() {
        let error = PORT.classify(
            "acquiring a connection",
            &DbErr::ConnectionAcquire(ConnAcquireErr::Timeout),
        );
        assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    }

    #[test]
    fn a_failed_statement_is_rejected_and_never_unavailable() {
        // The distinction the module exists for: a connection dropped *after* the server
        // committed is indistinguishable here from one dropped before, and `Unavailable`
        // promises the operation did not happen. Promising that wrongly is how a caller
        // retries a write that already landed.
        let error = PORT.classify(
            "recording a blob",
            &DbErr::Exec(RuntimeErr::Internal("connection reset".to_owned())),
        );
        assert!(matches!(error, StoreError::Rejected { .. }), "{error:?}");
    }

    #[test]
    fn a_value_that_will_not_decode_is_corrupt_and_names_the_record() {
        let error = PORT.classify("reading a row", &DbErr::Type("not an i64".to_owned()));
        match error {
            StoreError::Corrupt { record, .. } => assert_eq!(record, "TestRecord"),
            other => panic!("a decoding failure must be Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn an_adapter_raised_decoding_failure_is_the_same_variant() {
        // The adapters raise this themselves for an unknown enum discriminant or an address
        // that no longer parses — the driver cannot, because those columns are plain text.
        let error = PORT.undecodable("`elsewhere` is not an asset state");
        assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    }
}
