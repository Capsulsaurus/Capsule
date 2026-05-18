//! UniFFI record types mirroring the `capsule-core` catalog rows.
//!
//! Each record is a faithful 1:1 mirror of its `capsule-core` row struct. The
//! mirror types (rather than annotating `capsule-core` directly) keep
//! `capsule-core` free of FFI concerns and make the boundary contract explicit.

use capsule_core::db::{AlbumRow, AssetRow, AssetStackRow, StackMemberRow};

/// A row in the `assets` table — one imported media file.
///
/// `asset_type`, `capture_tz_source` carry their canonical snake_case string
/// values (`"photo"`, `"offset_exif"`, …) exactly as stored in SQLite.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AssetRecord {
    pub uuid: String,
    pub asset_type: String,
    pub capture_timestamp: i64,
    pub capture_utc: Option<i64>,
    pub capture_tz_source: Option<String>,
    pub import_timestamp: i64,
    pub hash_sha256: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub stack_id: Option<String>,
    pub is_stack_hidden: bool,
    pub chromahash: Option<String>,
    pub dominant_color: Option<String>,
    pub album_id: Option<String>,
    pub rating: i64,
    pub is_deleted: bool,
    pub deleted_at: Option<i64>,
}

impl From<AssetRow> for AssetRecord {
    fn from(r: AssetRow) -> Self {
        Self {
            uuid: r.uuid,
            asset_type: r.asset_type,
            capture_timestamp: r.capture_timestamp,
            capture_utc: r.capture_utc,
            capture_tz_source: r.capture_tz_source,
            import_timestamp: r.import_timestamp,
            hash_sha256: r.hash_sha256,
            width: r.width,
            height: r.height,
            duration_ms: r.duration_ms,
            stack_id: r.stack_id,
            is_stack_hidden: r.is_stack_hidden,
            chromahash: r.chromahash,
            dominant_color: r.dominant_color,
            album_id: r.album_id,
            rating: r.rating,
            is_deleted: r.is_deleted,
            deleted_at: r.deleted_at,
        }
    }
}

impl From<AssetRecord> for AssetRow {
    fn from(r: AssetRecord) -> Self {
        Self {
            uuid: r.uuid,
            asset_type: r.asset_type,
            capture_timestamp: r.capture_timestamp,
            capture_utc: r.capture_utc,
            capture_tz_source: r.capture_tz_source,
            import_timestamp: r.import_timestamp,
            hash_sha256: r.hash_sha256,
            width: r.width,
            height: r.height,
            duration_ms: r.duration_ms,
            stack_id: r.stack_id,
            is_stack_hidden: r.is_stack_hidden,
            chromahash: r.chromahash,
            dominant_color: r.dominant_color,
            album_id: r.album_id,
            rating: r.rating,
            is_deleted: r.is_deleted,
            deleted_at: r.deleted_at,
        }
    }
}

/// A row in the `asset_stacks` table — a group of related assets.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AssetStackRecord {
    pub id: String,
    pub stack_type: String,
    pub primary_asset_id: String,
    pub cover_asset_id: Option<String>,
    pub is_collapsed: bool,
    pub is_auto_generated: bool,
    pub created_at: i64,
    pub modified_at: i64,
}

impl From<AssetStackRow> for AssetStackRecord {
    fn from(r: AssetStackRow) -> Self {
        Self {
            id: r.id,
            stack_type: r.stack_type,
            primary_asset_id: r.primary_asset_id,
            cover_asset_id: r.cover_asset_id,
            is_collapsed: r.is_collapsed,
            is_auto_generated: r.is_auto_generated,
            created_at: r.created_at,
            modified_at: r.modified_at,
        }
    }
}

impl From<AssetStackRecord> for AssetStackRow {
    fn from(r: AssetStackRecord) -> Self {
        Self {
            id: r.id,
            stack_type: r.stack_type,
            primary_asset_id: r.primary_asset_id,
            cover_asset_id: r.cover_asset_id,
            is_collapsed: r.is_collapsed,
            is_auto_generated: r.is_auto_generated,
            created_at: r.created_at,
            modified_at: r.modified_at,
        }
    }
}

/// A row in the `stack_members` table — one asset's membership in a stack.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct StackMemberRecord {
    pub id: String,
    pub stack_id: String,
    pub asset_id: String,
    pub sequence_order: i64,
    pub member_role: String,
    pub created_at: i64,
}

impl From<StackMemberRow> for StackMemberRecord {
    fn from(r: StackMemberRow) -> Self {
        Self {
            id: r.id,
            stack_id: r.stack_id,
            asset_id: r.asset_id,
            sequence_order: r.sequence_order,
            member_role: r.member_role,
            created_at: r.created_at,
        }
    }
}

impl From<StackMemberRecord> for StackMemberRow {
    fn from(r: StackMemberRecord) -> Self {
        Self {
            id: r.id,
            stack_id: r.stack_id,
            asset_id: r.asset_id,
            sequence_order: r.sequence_order,
            member_role: r.member_role,
            created_at: r.created_at,
        }
    }
}

/// A row in the `albums` table — a user-defined album.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AlbumRecord {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub modified_at: i64,
    pub cover_asset_id: Option<String>,
}

impl From<AlbumRow> for AlbumRecord {
    fn from(r: AlbumRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            created_at: r.created_at,
            modified_at: r.modified_at,
            cover_asset_id: r.cover_asset_id,
        }
    }
}

impl From<AlbumRecord> for AlbumRow {
    fn from(r: AlbumRecord) -> Self {
        Self {
            id: r.id,
            name: r.name,
            created_at: r.created_at,
            modified_at: r.modified_at,
            cover_asset_id: r.cover_asset_id,
        }
    }
}
