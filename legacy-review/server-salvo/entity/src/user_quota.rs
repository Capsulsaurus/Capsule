//! Per-user quota lifecycle state (slice `S-C6`).
//!
//! One row per user, holding the two facts the accounting sums cannot derive by themselves:
//!
//! - `hard_exceeded_since` — when the user first crossed (and has since stayed at/above) the
//!   hard limit. The **Grace-expired** state is `hard_exceeded_since` older than the
//!   `grace_window`; the marker is set when a quota check observes the user at/above the hard
//!   limit with no marker, and cleared when a check observes them back under it. A `NULL`
//!   marker means "not currently hard-exceeded".
//! - `suspended` — an admin/billing (moderation) flag. The quota service only **reports** it
//!   (as [`QuotaState::Suspended`]); the enforcement of suspension at session creation is
//!   owned by the moderation slice, not here.
//!
//! Rows are created lazily (a user with no row is simply "never hard-exceeded, not
//! suspended"). SSoT: the [Quota design doc](../../../../capsule-docs/src/content/docs/design/quota.md).
//!
//! [`QuotaState::Suspended`]: /design/quota/#thresholds-and-states

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "user_quota")]
pub struct Model {
    /// The account id (matches `users.id`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: String,
    /// When the user first crossed the hard limit and has since stayed at/above it; `NULL`
    /// when not currently hard-exceeded. The grace window is measured from this instant.
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub hard_exceeded_since: Option<DateTime<Utc>>,
    /// Admin/billing suspension flag (moderation-owned enforcement; quota only reports it).
    #[sea_orm(default_value = "false")]
    pub suspended: bool,
    /// Last update instant.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
