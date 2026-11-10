/// The counter suite, and the races only a real backend can contest, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;

    use jiff::{SignedDuration, Timestamp};

    use super::super::PostgresCounters;
    use crate::counter::conformance as counters;
    use crate::counter::{Budget, CounterKey, CounterStore, is_at_capacity};
    use crate::postgres::testing;

    #[tokio::test]
    async fn the_counter_suite_passes_against_postgres() {
        let Some(database) = testing::start("the Postgres counter suite").await else {
            return;
        };
        let store: Arc<dyn CounterStore> =
            Arc::new(PostgresCounters::new(database.connection().clone()));
        counters::run_all(store).await;
    }

    #[tokio::test]
    async fn a_full_counter_partition_refuses_a_new_key_against_postgres() {
        let Some(database) = testing::start("the Postgres counter ceiling").await else {
            return;
        };
        let store =
            PostgresCounters::new(database.connection().clone()).with_ceiling(counters::CEILING);
        counters::a_full_partition_refuses_only_a_new_key_in_that_partition(&store).await;
    }

    /// Racing first hits on distinct keys fill the partition to its ceiling and no further.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_hits_race_to_the_partition_ceiling() {
        let Some(database) = testing::start("the Postgres counter ceiling race").await else {
            return;
        };
        let store: Arc<dyn CounterStore> = Arc::new(
            PostgresCounters::new(database.connection().clone()).with_ceiling(counters::CEILING),
        );
        let at = Timestamp::UNIX_EPOCH;
        let budget = Budget::new(5, SignedDuration::from_mins(1));
        let mut tasks = tokio::task::JoinSet::new();
        for n in 0..24 {
            let store = Arc::clone(&store);
            let key = CounterKey::DropSource(format!("postgres-race-ceiling-{n}"));
            tasks.spawn(async move { store.hit(&key, budget, at).await });
        }
        let (mut admitted, mut refused) = (0, 0);
        while let Some(outcome) = tasks.join_next().await {
            match outcome.expect("a hit task completes") {
                Ok(verdict) if verdict.admits() => admitted += 1,
                Ok(verdict) => panic!("a first hit is never rate-limited: {verdict:?}"),
                Err(error) => {
                    assert!(is_at_capacity(&error), "{error}");
                    refused += 1;
                }
            }
        }
        assert_eq!(
            (admitted, refused),
            (counters::CEILING, 24 - counters::CEILING)
        );
    }

    /// Forty tasks, one key, a budget of five: exactly five are admitted. With a limit of one this
    /// is the replay-nonce property — two copies of one signed request never both get through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn counter_hits_race_to_the_budget() {
        let Some(database) = testing::start("the Postgres counter budget race").await else {
            return;
        };
        let store: Arc<dyn CounterStore> =
            Arc::new(PostgresCounters::new(database.connection().clone()));
        for (limit, tasks_count) in [(5, 40), (1, 16)] {
            let key = CounterKey::ShareLink(format!("postgres-race-link-{limit}"));
            let budget = Budget::new(limit, SignedDuration::from_mins(1));
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..tasks_count {
                let store = Arc::clone(&store);
                let key = key.clone();
                tasks.spawn(async move {
                    store
                        .hit(&key, budget, Timestamp::UNIX_EPOCH)
                        .await
                        .expect("answers")
                        .admits()
                });
            }
            let mut admitted = 0;
            while let Some(admits) = tasks.join_next().await {
                if admits.expect("a hit task completes") {
                    admitted += 1;
                }
            }
            assert_eq!(admitted, limit, "a budget of {limit} admits exactly {limit}");
        }
    }
}
