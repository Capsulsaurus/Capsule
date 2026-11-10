/// The suite, against a real Postgres.
mod postgres_conformance {
    use jiff::Timestamp;
    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

    use super::super::PostgresSubscriptions;
    use crate::federation::PeerId;
    use crate::federation::capability::Scope;
    use crate::federation::subscriptions::{
        CapabilityToken, Subscription, SubscriptionStore, conformance,
    };
    use crate::postgres::testing;
    use crate::store::{AlbumId, UserId};

    #[tokio::test]
    async fn the_postgres_subscription_store_conforms() {
        let Some(database) = testing::start("the Postgres subscription store").await else {
            return;
        };
        let store = PostgresSubscriptions::new(database.connection().clone());
        conformance::run_all(&store).await;
        conformance::racing_capped_puts_record_one(std::sync::Arc::new(store)).await;
    }

    /// `subscriber`'s subscription to `album` on `peer`, live for a day from `issued`.
    fn subscription(subscriber: &UserId, peer: &PeerId, album: &AlbumId) -> Subscription {
        let issued = Timestamp::from_second(1_767_225_600).expect("a representable instant");
        let expires = Timestamp::from_second(1_767_312_000).expect("a representable instant");
        Subscription {
            subscriber: subscriber.clone(),
            peer: peer.clone(),
            album: album.clone(),
            scope: Scope::Read,
            jti: format!("{subscriber}-{album}"),
            token: CapabilityToken::new("token"),
            issued_at: issued,
            expires_at: expires,
            min_protocol_version: "2026-06-01".to_owned(),
            subscribed_at: issued,
            updated_at: issued,
        }
    }

    async fn execute(connection: &DatabaseConnection, sql: &str, values: Vec<Value>) {
        connection
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await
            .expect("a fixture write");
    }

    /// `user`'s stored pulled total, zero when the account has no usage row.
    async fn pulled(connection: &DatabaseConnection, user: &UserId) -> i64 {
        connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT pulled FROM quota_usage WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .expect("read the pulled total")
            .map_or(0, |row| row.try_get("", "pulled").expect("a pulled column"))
    }

    /// The store's own writes move the pulled total: the schema no longer does it for them.
    ///
    /// A job already holds a blob. Subscribing to it charges the subscriber the blob's size, a
    /// renewal under the held key charges nothing more, and unsubscribing releases it.
    #[tokio::test]
    async fn subscribing_and_unsubscribing_move_the_stored_pulled_total() {
        let Some(database) = testing::start("the subscription store's pulled total").await else {
            return;
        };
        let connection = database.connection().clone();
        let store = PostgresSubscriptions::new(connection.clone());
        let (alice, peer, album) = (
            UserId::new("wired-alice"),
            PeerId::new("wired.peer.test"),
            AlbumId::new("wired-album"),
        );
        let job = || {
            vec![
                Value::from(peer.as_str().to_owned()),
                Value::from(album.as_str().to_owned()),
            ]
        };
        execute(
            &connection,
            "INSERT INTO federation_pulled_albums (peer_id, album_id) VALUES ($1, $2)",
            job(),
        )
        .await;
        let mut blob = job();
        blob.extend([Value::from("wired-blob".to_owned()), Value::from(40_i64)]);
        execute(
            &connection,
            "INSERT INTO federation_pulled_metadata_blobs (peer_id, album_id, address, size) \
             VALUES ($1, $2, $3, $4)",
            blob,
        )
        .await;
        assert_eq!(pulled(&connection, &alice).await, 0, "nobody holds it yet");

        store
            .put(subscription(&alice, &peer, &album))
            .await
            .expect("subscribe");
        assert_eq!(
            pulled(&connection, &alice).await,
            40,
            "subscribing charged it"
        );

        store
            .put(subscription(&alice, &peer, &album))
            .await
            .expect("renew");
        assert_eq!(
            pulled(&connection, &alice).await,
            40,
            "a held key charges once"
        );

        assert!(
            store
                .remove(&alice, &peer, &album)
                .await
                .expect("unsubscribe"),
            "the subscription was held"
        );
        assert_eq!(
            pulled(&connection, &alice).await,
            0,
            "unsubscribing released it"
        );
    }
}
