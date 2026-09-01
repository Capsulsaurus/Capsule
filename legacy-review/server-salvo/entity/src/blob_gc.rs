//! Per-blob garbage-collection state (owned by the GC worker, slice `S-C11`; read by
//! storage verification, slice `S-C3`).
//!
//! One row per content-addressed ciphertext blob that is **not** in the ordinary live
//! state. The common case — a referenced, un-quarantined blob — carries no row at all, so
//! this table stays small and its absence encodes "live". The GC worker sets
//! `collectable_since` when a blob's refcount reaches zero (starting the grace clock) and
//! `quarantined` on an integrity fault; storage verification reads both to compute the
//! key-free `retrievable` fact.

use chrono::{DateTime, FixedOffset};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "blob_gc")]
pub struct Model {
    /// The blob's lowercase-hex ciphertext content address (64-char SHA-256).
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(64))"
    )]
    pub content_hash: String,
    /// When the blob became collectable (refcount reached 0). While set the blob is
    /// mid-collection and reports `retrievable = false`; its bytes survive until this
    /// instant plus the standing GC grace window (`service::gc::earliest_byte_deletion`).
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub collectable_since: Option<DateTime<FixedOffset>>,
    /// Whether an integrity fault has quarantined the blob (dangling reference / failed deep
    /// scan). A quarantined blob is never retrievable.
    #[sea_orm(default_value = "false")]
    pub quarantined: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
