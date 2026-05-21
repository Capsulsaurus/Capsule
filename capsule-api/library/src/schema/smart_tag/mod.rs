use async_graphql::*;
use entity::smart_tag::Model as SmartTagModel;
use entity::smart_tag::TagCategory as EntityTagCategory;

/// Category of AI-generated smart tags
#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum TagCategory {
    Scene,
    Object,
    Activity,
    Event,
    Weather,
    Color,
}

impl From<EntityTagCategory> for TagCategory {
    fn from(c: EntityTagCategory) -> Self {
        match c {
            EntityTagCategory::Scene => TagCategory::Scene,
            EntityTagCategory::Object => TagCategory::Object,
            EntityTagCategory::Activity => TagCategory::Activity,
            EntityTagCategory::Event => TagCategory::Event,
            EntityTagCategory::Weather => TagCategory::Weather,
            EntityTagCategory::Color => TagCategory::Color,
        }
    }
}

/// An AI-generated smart tag for categorizing assets
#[derive(SimpleObject)]
pub struct SmartTag {
    pub id: ID,
    pub name: String,
    pub category: TagCategory,
    /// Number of assets with this tag
    pub asset_count: i32,
}

impl SmartTag {
    pub fn from_model(model: SmartTagModel, asset_count: i32) -> Self {
        Self {
            id: ID::from(model.id.to_string()),
            name: model.name,
            category: model.category.into(),
            asset_count,
        }
    }
}
