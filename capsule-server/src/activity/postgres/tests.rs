use super::PostgresActivity;
use crate::activity::ActivityStore;
use crate::activity::conformance::{self, Harness};
use crate::postgres::testing;

/// A store over one container.
#[derive(Debug)]
struct PostgresHarness {
    activity: PostgresActivity,
}

impl Harness for PostgresHarness {
    fn activity(&self) -> &dyn ActivityStore {
        &self.activity
    }
}

/// The suite, against a real Postgres. Named for the container tier's `test(postgres_conformance)` filter.
#[tokio::test]
async fn postgres_conformance() {
    let Some(database) = testing::start("the Postgres activity store").await else {
        return;
    };
    let harness = PostgresHarness {
        activity: PostgresActivity::new(database.connection().clone()),
    };
    conformance::run_all(&harness).await;
}
