//! Known federated peer servers and their published Ed25519 signing keys (slice `S-C8`).
//!
//! A federated moderation report is signed by the reporting server's classical Ed25519
//! [operational key](https://docs/design/federation/#server-identity-and-key-rotation); the
//! report intake verifies its signature against the row for `server_id` before the report can
//! reach the admin queue. A peer with no row here is unknown — its report is unverifiable and
//! is dropped (invariant 24). SSoT: the [Moderation design doc](https://docs/design/moderation/).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "federation_peers")]
pub struct Model {
    /// The peer's canonical origin (e.g. `other.tld`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub server_id: String,
    /// The peer's 32-byte Ed25519 public signing key.
    #[sea_orm(column_type = "Blob")]
    pub ed25519_public_key: Vec<u8>,
    /// When this peer key was registered.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
