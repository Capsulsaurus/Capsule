use entity::user_quota;
use jiff::Timestamp;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Statement};

use super::{QuotaLimits, QuotaState, QuotaStatus};

pub struct Query;

impl Query {
    /// Total bytes charged to `user_id`: originals (from the `assets` index, first-uploader
    /// attributed, content-hash deduped) plus auxiliary/federated blobs (from the ledger).
    #[tracing::instrument(skip(db), fields(user_id = %user_id))]
    pub async fn used<C: ConnectionTrait>(db: &C, user_id: &str) -> Result<u64, DbErr> {
        let originals = Self::originals_used(db, user_id).await?;
        let auxiliary = Self::ledger_used(db, user_id).await?;
        let total = originals.saturating_add(auxiliary);
        tracing::debug!(originals, auxiliary, total, "quota usage computed");
        Ok(total)
    }

    /// Original-blob bytes charged to `user_id`. Each distinct `file_hash` is charged once, to
    /// its **first** uploader (earliest `uploaded_at`) — the content-addressed dedup
    /// attribution that stops a re-upload of another user's blob from inflating quota. Every
    /// present row counts at full size (a trash-retained asset still has a row; a hard-purged
    /// asset does not), so the accounting is honest without a trash/live flag.
    async fn originals_used<C: ConnectionTrait>(db: &C, user_id: &str) -> Result<u64, DbErr> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"SELECT COALESCE(SUM(sz), 0)::bigint AS total FROM (
                  SELECT DISTINCT ON (file_hash) file_size AS sz, upload_user_id
                  FROM assets
                  ORDER BY file_hash, uploaded_at ASC, id ASC
              ) firsts
              WHERE upload_user_id = $1",
            [user_id.into()],
        );
        Self::scalar_total(db, stmt).await
    }

    /// Auxiliary + federated bytes charged to `user_id` from the quota ledger (metadata,
    /// derivatives, provenance, federated caches). Garbage-collected rows are deleted, so a
    /// simple sum over present rows is the live total.
    async fn ledger_used<C: ConnectionTrait>(db: &C, user_id: &str) -> Result<u64, DbErr> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"SELECT COALESCE(SUM(byte_size), 0)::bigint AS total
              FROM quota_ledger WHERE attributed_user_id = $1",
            [user_id.into()],
        );
        Self::scalar_total(db, stmt).await
    }

    /// Federated bytes cached for `user_id` from a single `source_peer` — the per-peer caching
    /// budget denominator.
    #[tracing::instrument(skip(db), fields(user_id = %user_id, peer = %source_peer))]
    pub async fn used_from_peer<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        source_peer: &str,
    ) -> Result<u64, DbErr> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"SELECT COALESCE(SUM(byte_size), 0)::bigint AS total
              FROM quota_ledger WHERE attributed_user_id = $1 AND source_peer = $2",
            [user_id.into(), source_peer.into()],
        );
        Self::scalar_total(db, stmt).await
    }

    /// The persisted lifecycle facts for `user_id`: the grace clock and the suspension flag.
    /// A missing row means "never hard-exceeded, not suspended".
    pub async fn lifecycle_state<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<(Option<Timestamp>, bool), DbErr> {
        Ok(user_quota::Entity::find_by_id(user_id)
            .one(db)
            .await?
            .map_or((None, false), |row| {
                (
                    row.hard_exceeded_since.map(entity::time::entity_to_ts),
                    row.suspended,
                )
            }))
    }

    /// A user's full quota snapshot — the `GET /quota` payload — combining accounting with the
    /// persisted lifecycle state and the deployment `limits`.
    #[tracing::instrument(skip(db, limits), fields(user_id = %user_id))]
    pub async fn current_status<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        limits: &QuotaLimits,
    ) -> Result<QuotaStatus, DbErr> {
        let used = Self::used(db, user_id).await?;
        let (since, suspended) = Self::lifecycle_state(db, user_id).await?;
        let state = QuotaState::classify(used, limits, since, suspended, Timestamp::now());
        Ok(QuotaStatus {
            used,
            soft_limit: limits.soft_limit,
            hard_limit: limits.hard_limit,
            state,
        })
    }

    /// Run a `… AS total` bigint scalar query and coerce it to `u64` (sizes are non-negative).
    async fn scalar_total<C: ConnectionTrait>(db: &C, stmt: Statement) -> Result<u64, DbErr> {
        let total = db
            .query_one(stmt)
            .await?
            .map(|row| row.try_get::<i64>("", "total"))
            .transpose()?
            .unwrap_or(0);
        Ok(u64::try_from(total).unwrap_or(0))
    }
}
