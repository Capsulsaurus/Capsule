use super::*;

/// Every table migration `000031` added carries an expiry the sweeper reads. A state table not
/// listed here would grow forever with nothing reporting it.
#[test]
fn every_state_table_is_swept() {
    let migration = include_str!("../../../migration/src/m20260902_000031_volatile_state.rs");
    let created: Vec<&str> = migration
        .lines()
        .filter_map(|line| line.trim().strip_prefix("CREATE TABLE "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();
    let swept: Vec<&str> = SWEPT_TABLES.iter().map(|(table, _)| *table).collect();
    for table in &created {
        // The mailbox has no expiry of its own; it is deleted with its channel.
        if *table == "enrollment_mailbox" {
            continue;
        }
        assert!(swept.contains(table), "`{table}` is never swept");
    }
    for table in &swept {
        assert!(
            created.contains(table),
            "`{table}` is swept but not created by 000031"
        );
    }
}

#[test]
fn a_report_totals_its_tables() {
    let mut report = SweepReport::default();
    assert_eq!(report.total(), 0);
    report.removed.insert("auth_sessions", 3);
    report.removed.insert("rate_counters", 4);
    assert_eq!(report.total(), 7);
}

/// The sweep, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;

    use jiff::{SignedDuration, Timestamp};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    use super::super::ExpirySweeper;
    use crate::postgres::testing;
    use crate::store::memory::ManualClock;
    use crate::store::postgres::PostgresStores;
    use crate::store::{
        AuthStateStore, ChallengeStore, ChallengeToken, Clock, RevokeAllChallenge, SessionId,
        SessionRecord, UserId,
    };

    async fn count(database: &testing::TestDatabase, table: &str) -> i64 {
        let row = database
            .connection()
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT count(*) AS n FROM {table}"),
            ))
            .await
            .expect("counts")
            .expect("a row");
        row.try_get("", "n").expect("a count")
    }

    fn session(id: &str, at: Timestamp) -> SessionRecord {
        SessionRecord {
            session_id: SessionId::new(id),
            user_id: UserId::new("sweep-user"),
            created_at: at,
            authenticated_at: at,
            signed_in_at: at,
            last_active_at: at,
            user_agent: None,
            ip_address: None,
            cohort_hash: None,
            device_id: None,
        }
    }

    /// Expired rows go, in batches smaller than the backlog; live rows stay.
    #[tokio::test]
    async fn a_sweep_deletes_only_expired_rows_in_bounded_batches() {
        let Some(database) = testing::start("the Postgres expiry sweep").await else {
            return;
        };
        let clock = ManualClock::default();
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        let stores = PostgresStores::with_uniform_ttl(
            database.connection().clone(),
            Arc::clone(&shared),
            SignedDuration::from_mins(1),
        );
        let auth = stores.auth();
        let challenges = stores.challenges();
        for n in 0..7 {
            auth.open_session(session(&format!("old-{n}"), clock.now()))
                .await
                .expect("opens");
        }
        challenges
            .issue(
                &ChallengeToken::new("old-challenge"),
                RevokeAllChallenge {
                    user_id: UserId::new("sweep-user"),
                    issued_at: clock.now(),
                },
            )
            .await
            .expect("issues");
        clock.advance(SignedDuration::from_mins(2));
        auth.open_session(session("new", clock.now()))
            .await
            .expect("opens");

        let sweeper = ExpirySweeper::new(database.connection().clone(), shared).with_batch(3);
        let report = sweeper.sweep().await.expect("sweeps");
        assert_eq!(report.removed.get("auth_sessions"), Some(&7));
        assert_eq!(report.removed.get("revoke_all_challenges"), Some(&1));
        assert_eq!(report.total(), 8);
        assert_eq!(count(&database, "auth_sessions").await, 1);
        assert!(
            auth.read_session(&SessionId::new("new"))
                .await
                .expect("reads")
                .is_some(),
            "a live session survives the sweep"
        );

        let again = sweeper.sweep().await.expect("sweeps");
        assert_eq!(again.total(), 0, "a second sweep finds nothing");
    }
}
