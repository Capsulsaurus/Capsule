/// The suite, against a real Postgres.
mod postgres_conformance {
    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

    use super::super::PostgresQuota;
    use crate::blob::ContentAddress;
    use crate::postgres::testing;
    use crate::quota::QuotaStore;
    use crate::quota::conformance::{self, Harness};
    use crate::store::{StoreError, StoreFuture, UserId};

    /// A ledger over one container, with the connection its pulled holdings are written through.
    #[derive(Debug)]
    struct PostgresHarness {
        quotas: PostgresQuota,
        connection: DatabaseConnection,
    }

    /// The peer every conformance album is pulled from.
    const PEER: &str = "quota-conformance-peer";

    impl PostgresHarness {
        /// Run `sql` with `values`, as conformance setup.
        async fn run(&self, sql: &str, values: Vec<Value>) -> Result<(), StoreError> {
            self.connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    values,
                ))
                .await
                .map(|_| ())
                .map_err(|error| StoreError::Rejected {
                    store: "quota conformance",
                    detail: error.to_string(),
                })
        }
    }

    impl Harness for PostgresHarness {
        fn quotas(&self) -> &dyn QuotaStore {
            &self.quotas
        }

        fn hold_pulled<'a>(
            &'a self,
            user: &'a UserId,
            album: &'a str,
            address: &'a ContentAddress,
            size: u64,
        ) -> StoreFuture<'a, ()> {
            Box::pin(async move {
                let job = || vec![Value::from(PEER), Value::from(album.to_owned())];
                let mut subscription = vec![Value::from(user.as_str().to_owned())];
                subscription.extend(job());
                self.run(
                    "INSERT INTO federation_subscriptions (subscriber_id, peer_id, album_id, \
                     scope, jti, token, issued_at, expires_at, min_protocol_version, \
                     subscribed_at, updated_at) \
                     VALUES ($1, $2, $3, 'read', 'jti', 'token', 0, 0, '1.0', 0, 0) \
                     ON CONFLICT DO NOTHING",
                    subscription,
                )
                .await?;
                self.run(
                    "INSERT INTO federation_pulled_albums (peer_id, album_id) VALUES ($1, $2) \
                     ON CONFLICT DO NOTHING",
                    job(),
                )
                .await?;
                let mut blob = job();
                blob.extend([
                    Value::from(address.as_str().to_owned()),
                    Value::from(i64::try_from(size).expect("a test size fits a BIGINT")),
                ]);
                self.run(
                    "INSERT INTO federation_pulled_metadata_blobs (peer_id, album_id, address, \
                     size) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                    blob,
                )
                .await
            })
        }

        fn unsubscribe<'a>(&'a self, user: &'a UserId, album: &'a str) -> StoreFuture<'a, ()> {
            Box::pin(async move {
                self.run(
                    "DELETE FROM federation_subscriptions \
                     WHERE subscriber_id = $1 AND peer_id = $2 AND album_id = $3",
                    vec![
                        Value::from(user.as_str().to_owned()),
                        Value::from(PEER),
                        Value::from(album.to_owned()),
                    ],
                )
                .await
            })
        }
    }

    #[tokio::test]
    async fn the_postgres_ledger_conforms() {
        let Some(database) = testing::start("the Postgres quota ledger").await else {
            return;
        };
        let harness = PostgresHarness {
            quotas: PostgresQuota::new(database.connection().clone()),
            connection: database.connection().clone(),
        };
        conformance::run_all(&harness).await;

        // A suspension survives a restart (#705): a fresh pool and a fresh adapter, sharing
        // nothing with the one that wrote it but the database, read it back.
        let user = crate::store::UserId::new("restart-user-1");
        let at = jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(86_400);
        assert!(
            harness
                .quotas
                .suspend(&user, at)
                .await
                .expect("the ledger suspends")
        );
        let reopened = PostgresQuota::new(
            sea_orm::Database::connect(database.url())
                .await
                .expect("a second pool connects"),
        );
        assert_eq!(
            reopened
                .usage(&user)
                .await
                .expect("the ledger answers")
                .suspended_since,
            Some(at),
            "a suspension is durable, not a property of the process that recorded it"
        );
    }
}
