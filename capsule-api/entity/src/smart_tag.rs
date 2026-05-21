use sea_orm::entity::prelude::*;

/// Category of AI-generated smart tags
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(15))")]
pub enum TagCategory {
    #[sea_orm(string_value = "scene")]
    Scene, // beach, mountain, city
    #[sea_orm(string_value = "object")]
    Object, // dog, car, flower
    #[sea_orm(string_value = "activity")]
    Activity, // running, cooking, wedding
    #[sea_orm(string_value = "event")]
    Event, // birthday, christmas
    #[sea_orm(string_value = "weather")]
    Weather, // sunny, rainy, snowy
    #[sea_orm(string_value = "color")]
    Color, // red, blue (derived from dominant_color)
}

/// Represents an AI-generated smart tag for categorizing assets
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "smart_tags")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i32,
    /// The tag name (e.g., "beach", "sunset", "dog")
    #[sea_orm(unique, column_type = "String(StringLen::N(100))", indexed)]
    pub name: String,
    /// Category of the tag
    pub category: TagCategory,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::asset_smart_tag::Entity")]
    AssetSmartTags,
}

impl Related<super::asset_smart_tag::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AssetSmartTags.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Entity {
    pub fn find_by_category(category: TagCategory) -> Select<Entity> {
        Self::find().filter(Column::Category.eq(category))
    }

    pub fn find_by_name(name: &str) -> Select<Entity> {
        Self::find().filter(Column::Name.eq(name))
    }
}
