/// The suite, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;

    use super::super::{PostgresCapabilities, PostgresPeers};
    use crate::federation::conformance::{self, Harness};
    use crate::federation::{CapabilityStore, PeerStore};
    use crate::postgres::testing;
    use crate::store::memory::ManualClock;

    /// Both stores over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        clock: Arc<ManualClock>,
        capabilities: PostgresCapabilities,
        peers: PostgresPeers,
    }

    impl Harness for PostgresHarness {
        fn capabilities(&self) -> &dyn CapabilityStore {
            &self.capabilities
        }

        fn peers(&self) -> &dyn PeerStore {
            &self.peers
        }

        fn clock(&self) -> &ManualClock {
            &self.clock
        }
    }

    #[tokio::test]
    async fn the_postgres_federation_stores_conform() {
        let Some(database) = testing::start("the Postgres federation stores").await else {
            return;
        };
        let clock = Arc::new(ManualClock::default());
        let harness = PostgresHarness {
            capabilities: PostgresCapabilities::new(database.connection().clone(), clock.clone()),
            peers: PostgresPeers::new(database.connection().clone()),
            clock,
        };
        conformance::run_all(&harness).await;
    }
}
