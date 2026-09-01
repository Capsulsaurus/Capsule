//! The lifecycle-write content-hash replay store (slice `S-C16`).
//!
//! One row per accepted `POST /albums/{album_id}/ops`, keyed by the SHA-256 of the signed op
//! bundle (canonical-CBOR manifest ‖ metadata blob). The row remembers the **byte-identical**
//! response the first acceptance produced; a resubmission of the exact bundle short-circuits
//! to it. Written inside the finalization transaction that appends the provenance record and
//! mints the `sync_seq`, so the op is applied at most once (the lifecycle analogue of the
//! upload chunk-replay tuple).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "lifecycle_op_replay")]
pub struct Model {
    /// The op bundle's content address (lowercase hex SHA-256) — the idempotency key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub op_hash: String,
    /// The album the op targeted.
    pub album_id: String,
    /// The signed manifest's `file_id` (the asset the op chained onto).
    pub asset_id: String,
    /// The accepted lifecycle action's stored `ChangeKind` discriminant.
    pub action: i16,
    /// The HTTP status the first acceptance returned.
    pub status_code: i32,
    /// The byte-identical JSON response body the first acceptance returned.
    #[sea_orm(column_type = "Blob")]
    pub response_body: Vec<u8>,
    /// Row creation instant.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
