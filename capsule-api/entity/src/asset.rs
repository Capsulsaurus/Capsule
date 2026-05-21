use chrono::{DateTime, FixedOffset, Utc};
use nanoid::nanoid;
use sea_orm::Set;
use sea_orm::entity::prelude::*;

// TODO: Check
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "assets")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: String,
    #[sea_orm(indexed)]
    pub owner_id: String,
    #[sea_orm(nullable, indexed)]
    pub album_id: Option<String>,

    /// Width of asset
    pub width: i32,
    /// Height of asset
    pub height: i32,

    pub asset_type: AssetType,

    /// Original filename from client
    pub original_filename: String,
    /// File size in bytes
    pub file_size: i64,
    /// BLAKE3 hash of the file content (64-char lowercase hex)
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub file_hash: String,
    /// MIME type
    pub content_type: String,

    // ===== Geo-location (for efficient spatial queries) =====
    /// GPS latitude
    #[sea_orm(column_type = "Double", nullable)]
    pub latitude: Option<f64>,
    /// GPS longitude  
    #[sea_orm(column_type = "Double", nullable)]
    pub longitude: Option<f64>,

    // ===== Visual placeholders =====
    /// Low Quality Image Placeholder hash for instant loading
    #[sea_orm(column_type = "String(StringLen::N(50))", nullable)]
    pub lqip_hash: Option<String>,
    /// Dominant color hex code (e.g., "#FF5733")
    #[sea_orm(column_type = "String(StringLen::N(7))", nullable)]
    pub dominant_color: Option<String>,

    // ===== User preferences =====
    /// Whether this asset is marked as favorite
    #[sea_orm(default_value = "false", indexed)]
    pub is_favorite: bool,

    // ===== Stack membership (new) =====
    /// If part of a stack, the stack ID (for fast lookup)
    /// An asset can only belong to one stack at a time
    #[sea_orm(nullable, indexed)]
    pub stack_id: Option<String>,

    /// Whether this asset is hidden when viewing the parent stack collapsed
    /// Primary asset has this = false, alternates have this = true
    #[sea_orm(default_value = "false")]
    pub is_stack_hidden: bool,

    // ===== Timestamps =====
    /// Date when the asset was captured/taken (from EXIF DateTimeOriginal)
    #[sea_orm(indexed)]
    pub captured_at: Option<DateTime<Utc>>,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP",
        indexed
    )]
    pub uploaded_at: DateTime<Utc>,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP",
        on_update = "CURRENT_TIMESTAMP",
        indexed
    )]
    pub modified_at: DateTime<FixedOffset>,
    /// Date when the album was deleted if not NULL
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable, indexed)]
    pub deleted_at: Option<DateTime<Utc>>,
    /// Whether asset has been uploaded
    pub uploaded: bool,
    /// User who uploaded the asset (for storage quota)
    #[sea_orm(indexed)]
    pub upload_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(2))")]
pub enum AssetType {
    #[sea_orm(string_value = "ph")]
    Photo,
    #[sea_orm(string_value = "vi")]
    Video,
    #[sea_orm(string_value = "mp")]
    MotionPhoto,
    #[sea_orm(string_value = "sc")]
    Sidecar,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::owner::Entity",
        from = "Column::OwnerId",
        to = "super::owner::Column::Id"
    )]
    Owner,
    #[sea_orm(
        belongs_to = "super::album::Entity",
        from = "Column::AlbumId",
        to = "super::album::Column::Id"
    )]
    Album,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UploadUserId",
        to = "super::user::Column::Id"
    )]
    UploadUser,
    #[sea_orm(
        belongs_to = "super::asset_stack::Entity",
        from = "Column::StackId",
        to = "super::asset_stack::Column::Id"
    )]
    Stack,
}

impl Related<super::owner::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::album::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Album.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::UploadUser.def()
    }
}

impl Related<super::asset_stack::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Stack.def()
    }
}

impl ActiveModelBehavior for ActiveModel {
    fn new() -> Self {
        Self {
            id: Set(nanoid!()),
            ..ActiveModelTrait::default()
        }
    }
}

impl Entity {
    pub fn find_by_owner_id(owner_id: &str) -> Select<Entity> {
        Self::find().filter(Column::OwnerId.eq(owner_id))
    }

    pub fn find_by_album_id(album_id: &str) -> Select<Entity> {
        Self::find().filter(Column::AlbumId.eq(album_id))
    }

    pub fn find_by_owner_id_and_album_id(owner_id: &str, album_id: &str) -> Select<Entity> {
        Self::find()
            .filter(Column::OwnerId.eq(owner_id))
            .filter(Column::AlbumId.eq(album_id))
    }
}
