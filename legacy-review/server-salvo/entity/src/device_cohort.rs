//! The durable device-cohort map (slice `S-C13`). One row per `(user_id, cohort_hash)`
//! holding the advisory session-grouping aid from the Authentication — Device Cohorts
//! contract.
//!
//! `cohort_hash` is client-asserted and **unverifiable**: it is stored verbatim and read
//! back only to group a user's sessions in the listing surface. No authorization or
//! capability decision reads it — the security-bearing identity is `device_id`/the DSK, kept
//! in an entirely separate identifier space. The table outlives session expiry so a reinstall
//! (fresh `device_id`, same cohort) can still be recognised as "a device you've used before".
//!
//! `user_id` is the account id (a nanoid, matching `users.id`) the session was created under.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "device_cohorts")]
pub struct Model {
    /// Account id (nanoid) the cohort was observed under — part of the composite key.
    #[sea_orm(primary_key, column_type = "Char(Some(21))", auto_increment = false)]
    pub user_id: String,
    /// The advisory, client-asserted cohort hash (opaque string). Never interpreted for
    /// any authorization decision — a grouping aid only.
    #[sea_orm(primary_key, auto_increment = false)]
    pub cohort_hash: String,
    /// First observation of this `(user, cohort)` — pinned, never moved back.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub first_seen: DateTime<Utc>,
    /// Most recent observation — bumped on every re-observation.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub last_seen: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
