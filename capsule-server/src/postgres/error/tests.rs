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

/// The driver-raised half, against a real Postgres: a `23505` cannot be built without one,
/// because sea-orm reads the SQLSTATE off the driver's own database-error type.
mod postgres_conformance {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    use super::PORT;
    use crate::postgres::testing;
    use crate::store::StoreError;

    #[tokio::test]
    async fn a_unique_violation_is_a_conflict_and_any_other_refusal_is_rejected() {
        let Some(database) = testing::start("the Postgres error classification").await else {
            return;
        };
        let insert = || {
            Statement::from_string(
                DbBackend::Postgres,
                "INSERT INTO owner_sequences (owner_id, next_seq) VALUES ('owner-523', 1)",
            )
        };
        database
            .connection()
            .execute_raw(insert())
            .await
            .expect("the first row is new");

        let duplicate = database
            .connection()
            .execute_raw(insert())
            .await
            .map_err(PORT.failing("inserting a held key"))
            .expect_err("the primary key holds the second row");
        assert!(
            matches!(duplicate, StoreError::Conflict { store: "test", .. }),
            "a unique violation must be a Conflict, got {duplicate:?}"
        );

        // A statement Postgres refuses for any other reason keeps its old reading: the
        // conflict arm is the SQLSTATE, not "the database said no".
        let missing = database
            .connection()
            .execute_raw(Statement::from_string(
                DbBackend::Postgres,
                "INSERT INTO no_such_table (id) VALUES (1)",
            ))
            .await
            .map_err(PORT.failing("inserting into a missing table"))
            .expect_err("the table does not exist");
        assert!(
            matches!(missing, StoreError::Rejected { .. }),
            "a non-unique refusal must stay Rejected, got {missing:?}"
        );
    }
}
