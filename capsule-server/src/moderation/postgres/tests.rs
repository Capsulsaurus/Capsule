use super::action_from;
use crate::moderation::ModerationAction;

/// Every action round-trips through its column, and nothing else decodes.
#[test]
fn every_action_round_trips_through_its_column() {
    for action in [
        ModerationAction::Suspended,
        ModerationAction::Reinstated,
        ModerationAction::TakenDown,
        ModerationAction::LegalHold,
        ModerationAction::HoldLifted,
    ] {
        assert_eq!(action_from(action.as_str()), Some(action));
    }
    assert_eq!(action_from("banished"), None);
}

/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresModeration;
    use crate::moderation::ModerationStore;
    use crate::moderation::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        moderation: PostgresModeration,
    }

    impl Harness for PostgresHarness {
        fn moderation(&self) -> &dyn ModerationStore {
            &self.moderation
        }
    }

    #[tokio::test]
    async fn the_postgres_moderation_store_conforms() {
        let Some(database) = testing::start("the Postgres moderation store").await else {
            return;
        };
        let harness = PostgresHarness {
            moderation: PostgresModeration::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
