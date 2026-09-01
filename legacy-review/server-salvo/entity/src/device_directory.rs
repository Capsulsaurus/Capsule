//! The signed device-directory store (slice `S-C9`). One row per user holding the latest
//! master-signed [`DeviceDirectory`] document as opaque canonical CBOR.
//!
//! The server treats `document` as an opaque blob it stores and serves verbatim — it never
//! re-models the signed bytes. Only `directory_version` is projected out of the document at
//! publish time, so the anti-rollback monotonicity check (threat-model invariant 23) can
//! refuse a non-advancing or regressing publish under the row's lock.
//!
//! `user_id` is the account id (a nanoid, matching `users.id`) the publisher authenticated
//! as, not the crypto `DirectoryCore.user_id`.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "device_directory")]
pub struct Model {
    /// Account id (nanoid) the directory belongs to — one directory per user.
    #[sea_orm(primary_key, column_type = "Char(Some(21))", auto_increment = false)]
    pub user_id: String,
    /// The strictly-monotonic version projected from the signed document (invariant 23).
    pub directory_version: i64,
    /// The signed `DeviceDirectory` as opaque canonical CBOR, stored verbatim.
    #[sea_orm(column_type = "Blob")]
    pub document: Vec<u8>,
    /// Instant the current directory was published.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
