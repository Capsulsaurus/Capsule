use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{Set, entity::prelude::*};

/// Represents an auto-generated memory (e.g., "On This Day", trip highlights)
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "memories")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,
    /// Owner of this memory
    #[sea_orm(indexed)]
    pub owner_id: String,
    /// Title (e.g., "3 Years Ago Today")
    #[sea_orm(column_type = "String(StringLen::N(255))")]
    pub title: String,
    /// Optional subtitle (e.g., "Trip to Paris")
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub subtitle: Option<String>,
    /// ID of the cover asset
    pub cover_asset_id: String,
    /// JSON array of asset IDs included in this memory
    #[sea_orm(column_type = "Text")]
    pub asset_ids: String,
    /// The historical date this memory references
    #[sea_orm(column_type = "TimestampWithTimeZone", indexed)]
    pub memory_date: DateTime<Utc>,
    /// Whether the user has seen this memory
    #[sea_orm(default_value = "false")]
    pub is_seen: bool,
    /// Whether the user has hidden this memory
    #[sea_orm(default_value = "false")]
    pub is_hidden: bool,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
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
        belongs_to = "super::asset::Entity",
        from = "Column::CoverAssetId",
        to = "super::asset::Column::Id"
    )]
    CoverAsset,
}

impl Related<super::owner::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::asset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CoverAsset.def()
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

    pub fn find_unseen_by_owner_id(owner_id: &str) -> Select<Entity> {
        Self::find()
            .filter(Column::OwnerId.eq(owner_id))
            .filter(Column::IsSeen.eq(false))
            .filter(Column::IsHidden.eq(false))
    }
}
