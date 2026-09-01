//! The durable federation-capability revocation list (slice `S-E2`).
//!
//! One row per revoked capability `jti`. The issuing server publishes the active rows as its
//! [`/.well-known/capsule/revoked-jti`](https://docs/design/federation/#token-lifecycle-and-chain-of-trust)
//! list and consults them when verifying its own tokens. A revoked-but-not-yet-expired token
//! is honored for at most the peers' 15-minute cache staleness bound; a row is **pruned** once
//! its `exp` passes (an expired token is rejected unconditionally anyway), so the list stays
//! bounded by at most 24 hours of revocations. SSoT: the
//! [Federation design doc](https://docs/design/federation/).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "federation_revoked_jti")]
pub struct Model {
    /// The revoked capability's `jti` (UUIDv7), the revocation key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub jti: String,
    /// The revoked token's `exp` (RFC 3339). The row is pruned once this passes.
    pub expires_at: DateTime<Utc>,
    /// When the revocation was recorded.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub revoked_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
