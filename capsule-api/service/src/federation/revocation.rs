//! The durable capability-revocation list (slice `S-E2`).
//!
//! Revocation is a short TTL (`exp ≤ 24h`) plus a published revocation list. This is the
//! issuer-side durable store behind that list: revoking a token records its `jti` (with the
//! token's `exp`), and the issuer consults it when verifying its own always-fresh tokens. A row
//! is **pruned** once its `exp` passes — an expired token is rejected unconditionally anyway — so
//! the published list is bounded by at most 24 hours of revocations. SSoT: the
//! [Federation design doc](https://docs/design/federation/#token-lifecycle-and-chain-of-trust).

use entity::federation_revoked_jti;
use jiff::Timestamp;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, Set};
use tracing::instrument;

/// Read/write surface over the durable `federation_revoked_jti` list.
pub struct Revocations;

impl Revocations {
    /// Revoke a capability by `jti`, recording the token's `exp` so the row can later be pruned.
    /// Idempotent — re-revoking the same `jti` is a no-op (the earliest revocation stands).
    #[instrument(skip(db), fields(jti = %jti))]
    pub async fn revoke<C: ConnectionTrait>(
        db: &C,
        jti: &str,
        expires_at: Timestamp,
    ) -> Result<(), DbErr> {
        federation_revoked_jti::Entity::insert(federation_revoked_jti::ActiveModel {
            jti: Set(jti.to_string()),
            expires_at: Set(entity::time::ts_to_entity(expires_at)),
            revoked_at: Set(entity::time::now_entity()),
        })
        .on_conflict(
            OnConflict::column(federation_revoked_jti::Column::Jti)
                .do_nothing()
                .to_owned(),
        )
        .do_nothing()
        .exec(db)
        .await?;
        tracing::info!(jti, "federation capability revoked");
        Ok(())
    }

    /// Whether `jti` is currently on the revocation list. (Callers still reject an expired token
    /// unconditionally; this is the not-yet-expired revocation check.)
    #[instrument(skip(db), fields(jti = %jti))]
    pub async fn is_revoked<C: ConnectionTrait>(db: &C, jti: &str) -> Result<bool, DbErr> {
        Ok(federation_revoked_jti::Entity::find_by_id(jti)
            .one(db)
            .await?
            .is_some())
    }

    /// The active (not-yet-expired) revoked `jti`s as of `now` — the body of the published
    /// `/.well-known/capsule/revoked-jti` document. Pruned entries never appear.
    #[instrument(skip(db))]
    pub async fn active_jtis<C: ConnectionTrait>(
        db: &C,
        now: Timestamp,
    ) -> Result<Vec<String>, DbErr> {
        federation_revoked_jti::Entity::find()
            .filter(federation_revoked_jti::Column::ExpiresAt.gt(entity::time::ts_to_entity(now)))
            .select_only()
            .column(federation_revoked_jti::Column::Jti)
            .into_tuple()
            .all(db)
            .await
    }

    /// Prune every revocation whose `exp` has passed. Returns the number of rows dropped. Keeps
    /// the published list bounded by at most 24 hours of revocations.
    #[instrument(skip(db))]
    pub async fn prune<C: ConnectionTrait>(db: &C, now: Timestamp) -> Result<u64, DbErr> {
        let deleted = federation_revoked_jti::Entity::delete_many()
            .filter(federation_revoked_jti::Column::ExpiresAt.lte(entity::time::ts_to_entity(now)))
            .exec(db)
            .await?
            .rows_affected;
        if deleted > 0 {
            tracing::info!(pruned = deleted, "pruned expired revoked jtis");
        }
        Ok(deleted)
    }
}
