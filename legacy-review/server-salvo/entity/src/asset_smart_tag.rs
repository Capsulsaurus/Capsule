use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

/// Junction table linking assets to smart tags (many-to-many)
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "asset_smart_tags")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i32,
    #[sea_orm(indexed)]
    pub asset_id: String,
    #[sea_orm(indexed)]
    pub smart_tag_id: i32,
    /// Confidence score from AI (0.0 to 1.0)
    #[sea_orm(column_type = "Double")]
    pub confidence: f64,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::asset::Entity",
        from = "Column::AssetId",
        to = "super::asset::Column::Id"
    )]
    Asset,
    #[sea_orm(
        belongs_to = "super::smart_tag::Entity",
        from = "Column::SmartTagId",
        to = "super::smart_tag::Column::Id"
    )]
    SmartTag,
}

impl Related<super::asset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Asset.def()
    }
}

impl Related<super::smart_tag::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SmartTag.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Entity {
    pub fn find_by_asset_id(asset_id: &str) -> Select<Entity> {
        Self::find().filter(Column::AssetId.eq(asset_id))
    }

    pub fn find_by_tag_id(tag_id: i32) -> Select<Entity> {
        Self::find().filter(Column::SmartTagId.eq(tag_id))
    }
}
