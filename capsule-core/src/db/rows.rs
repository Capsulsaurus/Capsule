#[derive(Debug, Clone, PartialEq)]
pub struct AssetRow {
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

#[derive(Debug, Clone, PartialEq)]
pub struct AssetStackRow {
    pub id: String,
    pub stack_type: String,
    pub primary_asset_id: String,
    pub cover_asset_id: Option<String>,
    pub is_collapsed: bool,
    pub is_auto_generated: bool,
    pub created_at: i64,
    pub modified_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StackMemberRow {
    pub id: String,
    pub stack_id: String,
    pub asset_id: String,
    pub sequence_order: i64,
    pub member_role: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssetTagRow {
    pub uuid: String,
    pub tag: String,
}

/// A user-defined album. Membership is tracked via `assets.album_id`
/// (one album per asset, per the filesystem design doc).
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRow {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub modified_at: i64,
    pub cover_asset_id: Option<String>,
}

/// One cached, reclaimable representation of an asset. The eviction sweep ranks these by
/// `last_accessed_at` (LRU) with `tier` as the tiebreaker; `pinned` and `is_owned_original`
/// rows are exempt. `path` is the on-disk cache file the sweep deletes.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedRepresentationRow {
    pub uuid: String,
    /// `"original"` | `"preview"` | `"thumbnail"`.
    pub tier: String,
    pub format: Option<String>,
    pub bytes: i64,
    pub path: String,
    pub last_accessed_at: i64,
    pub pinned: bool,
    /// An original this device itself owns as source of truth — never auto-evicted.
    pub is_owned_original: bool,
}
