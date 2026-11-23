/// The pull-job suite, and the claim race, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;

    use jiff::Timestamp;

    use super::super::PostgresPullJobs;
    use crate::federation::PeerId;
    use crate::federation::pull::{JobKey, PULL_LEASE, PullJobs, conformance};
    use crate::postgres::testing;
    use crate::store::AlbumId;

    #[tokio::test]
    async fn the_pull_job_suite_passes_against_postgres() {
        let Some(database) = testing::start("the Postgres pull-job suite").await else {
            return;
        };
        conformance::run_all(&PostgresPullJobs::new(database.connection().clone())).await;
    }

    /// Twenty-four replicas claim one due job at once; exactly one gets it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pull_job_claims_race_to_one_holder() {
        let Some(database) = testing::start("the Postgres pull-job claim race").await else {
            return;
        };
        let store = Arc::new(PostgresPullJobs::new(database.connection().clone()));
        let job = JobKey::new(
            PeerId::new("race.pull.test"),
            AlbumId::new(uuid::Uuid::now_v7().to_string()),
        );
        let now = Timestamp::now();
        let mut tasks = tokio::task::JoinSet::new();
        for replica in 0..24 {
            let store = Arc::clone(&store);
            let job = job.clone();
            tasks.spawn(async move {
                store
                    .claim(&job, &format!("replica-{replica}"), now, PULL_LEASE)
                    .await
                    .expect("the claim is answered")
                    .is_some()
            });
        }
        let mut admitted = 0;
        while let Some(claimed) = tasks.join_next().await {
            if claimed.expect("a claim task completes") {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 1, "two replicas never pull one job");
    }
}
