/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresDeviceDirectory;
    use crate::directory::DeviceDirectoryStore;
    use crate::directory::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        directories: PostgresDeviceDirectory,
    }

    impl Harness for PostgresHarness {
        fn directories(&self) -> &dyn DeviceDirectoryStore {
            &self.directories
        }
    }

    #[tokio::test]
    async fn the_postgres_device_directory_store_conforms() {
        let Some(database) = testing::start("the Postgres device-directory store").await else {
            return;
        };
        let harness = PostgresHarness {
            directories: PostgresDeviceDirectory::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
