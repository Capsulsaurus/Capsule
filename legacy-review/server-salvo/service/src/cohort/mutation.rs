use sea_orm::{ConnectionTrait, Statement};

use super::CohortError;

pub struct Mutation;

impl Mutation {
    /// Record a sighting of `(user_id, cohort_hash)` in the durable cohort map.
    ///
    /// A single guarded upsert pins `first_seen` on the first observation and bumps
    /// `last_seen` to the server clock on every re-observation; the row survives session
    /// expiry so a later reinstall (fresh `device_id`, same cohort) is still recognisable.
    ///
    /// **Advisory:** the caller records but does not gate on the result — a store failure is
    /// logged and swallowed by the auth ceremony, never surfaced. `cohort_hash` is stored
    /// **verbatim**; it is never interpreted for any authorization decision.
    #[tracing::instrument(skip(db), fields(user_id = %user_id))]
    pub async fn observe<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        cohort_hash: &str,
    ) -> Result<(), CohortError> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"INSERT INTO device_cohorts (user_id, cohort_hash, first_seen, last_seen)
              VALUES ($1, $2, now(), now())
              ON CONFLICT (user_id, cohort_hash) DO UPDATE
                 SET last_seen = now()",
            [user_id.into(), cohort_hash.into()],
        );
        db.execute(stmt).await?;
        tracing::debug!("device cohort observed");
        Ok(())
    }
}
