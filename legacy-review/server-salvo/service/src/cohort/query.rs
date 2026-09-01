use entity::device_cohort;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use super::CohortError;

/// One entry of a user's durable cohort map, as surfaced to the session-listing client.
///
/// Timestamps are Unix seconds (matching the session listing's other time fields), converted
/// from the stored `timestamptz` at this boundary so the transport layer never sees a
/// `chrono` type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortObservation {
    /// The advisory cohort hash (opaque; the client groups its sessions by this).
    pub cohort_hash: String,
    /// First time this cohort was seen for the user (Unix seconds).
    pub first_seen: i64,
    /// Most recent time this cohort was seen for the user (Unix seconds).
    pub last_seen: i64,
}

pub struct Query;

impl Query {
    /// The durable cohort map for `user_id`, oldest `first_seen` first.
    ///
    /// Read-only and non-authoritative: this is surfaced purely so the client can group the
    /// session ledger ("a device you've used before, last seen …"). It gates nothing.
    #[tracing::instrument(skip(db), fields(user_id = %user_id))]
    pub async fn for_user<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<Vec<CohortObservation>, CohortError> {
        let rows = device_cohort::Entity::find()
            .filter(device_cohort::Column::UserId.eq(user_id))
            .order_by_asc(device_cohort::Column::FirstSeen)
            .all(db)
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| CohortObservation {
                cohort_hash: row.cohort_hash,
                first_seen: row.first_seen.timestamp(),
                last_seen: row.last_seen.timestamp(),
            })
            .collect())
    }
}
