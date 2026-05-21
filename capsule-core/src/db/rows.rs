#[derive(Debug, Clone, PartialEq)]
pub struct AssetRow {
    pub uuid: String,
    pub asset_type: String,
    pub capture_timestamp: i64,
    pub capture_utc: Option<i64>,
    pub capture_tz_source: Option<String>,
    pub import_timestamp: i64,
    pub hash_blake3: String,
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
