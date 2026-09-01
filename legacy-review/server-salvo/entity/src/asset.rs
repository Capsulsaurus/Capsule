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

    /// Coarse media kind (photo/video/motion-photo/sidecar), mechanically derived from
    /// `content_type` — carries no capture-time, dimensional, locational, or user-authored
    /// information, so it is not plaintext user metadata (S-G3 key-free row set).
    pub asset_type: AssetType,

    /// Declared ciphertext size in bytes (quota attribution).
    pub file_size: i64,
    /// SHA-256 hash of the file content (64-char lowercase hex) — the blob content address.
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub file_hash: String,
    /// MIME type
    pub content_type: String,

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

    // ===== Moderation (S-C8) =====
    /// Whether this asset is servable. A takedown flips it `false`; a federated fetch then
    /// returns `410 Gone`. The underlying blob is never deleted — takedown is a serving
    /// constraint, not destruction (moderation doc). Default `true`.
    #[sea_orm(default_value = "true")]
    pub served: bool,
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
