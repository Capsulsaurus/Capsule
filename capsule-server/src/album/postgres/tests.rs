/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresAlbums;
    use crate::album::AlbumStore;
    use crate::album::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        albums: PostgresAlbums,
    }

    impl Harness for PostgresHarness {
        fn albums(&self) -> &dyn AlbumStore {
            &self.albums
        }
    }

    #[tokio::test]
    async fn the_postgres_album_store_conforms() {
        let Some(database) = testing::start("the Postgres album store").await else {
            return;
        };
        let harness = PostgresHarness {
            albums: PostgresAlbums::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
