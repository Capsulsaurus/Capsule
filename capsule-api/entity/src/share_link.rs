use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{Set, entity::prelude::*};

/// Type of content being shared via a public link
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(15))")]
pub enum ShareLinkType {
    #[sea_orm(string_value = "album")]
    Album,
    #[sea_orm(string_value = "asset")]
    Asset,
    #[sea_orm(string_value = "selection")]
    Selection,
}

/// Represents a public share link for sharing content externally
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "share_links")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,
    /// URL-safe unique token for the share link
    #[sea_orm(unique, column_type = "String(StringLen::N(32))", indexed)]
    pub token: String,
    /// User who created the share link
    #[sea_orm(indexed)]
    pub creator_id: String,
    /// Type of content being shared
    pub share_type: ShareLinkType,
    /// Target ID - album_id for Album type, or comma-separated asset IDs for Selection
    #[sea_orm(column_type = "Text")]
    pub target_id: String,
    /// Whether downloads are allowed
    #[sea_orm(default_value = "true")]
    pub allow_download: bool,
    /// Optional password hash for protected links
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub password_hash: Option<String>,
    /// Optional expiration timestamp
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Number of times the link has been viewed
    #[sea_orm(default_value = "0")]
    pub view_count: i32,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatorId",
        to = "super::user::Column::Id"
    )]
    Creator,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Creator.def()
    }
}

impl ActiveModelBehavior for ActiveModel {
    fn new() -> Self {
        Self {
            id: Set(nanoid!()),
            token: Set(nanoid!(32)), // Generate unique token
            ..ActiveModelTrait::default()
        }
    }
}

impl Entity {
    pub fn find_by_token(token: &str) -> Select<Entity> {
        Self::find().filter(Column::Token.eq(token))
    }

    pub fn find_by_creator_id(creator_id: &str) -> Select<Entity> {
        Self::find().filter(Column::CreatorId.eq(creator_id))
    }

    pub fn find_active() -> Select<Entity> {
        Self::find().filter(
            Column::ExpiresAt
                .is_null()
                .or(Column::ExpiresAt.gt(chrono::Utc::now())),
        )
    }
}
