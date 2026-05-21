use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{Set, entity::prelude::*};

/// Represents a detected face within an asset
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "faces")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,
    /// ID of the asset containing this face
    #[sea_orm(indexed)]
    pub asset_id: String,
    /// ID of the person this face is assigned to (null if unassigned)
    #[sea_orm(nullable, indexed)]
    pub person_id: Option<String>,
    /// Bounding box as JSON: {"x": 0.1, "y": 0.2, "width": 0.3, "height": 0.4} (normalized 0-1)
    #[sea_orm(column_type = "Text")]
    pub bounding_box: String,
    /// Face embedding vector for recognition (stored as binary, typically 512 bytes)
    #[sea_orm(column_type = "VarBinary(StringLen::N(2048))", nullable)]
    pub embedding: Option<Vec<u8>>,
    /// Detection confidence score (0.0 to 1.0)
    #[sea_orm(column_type = "Double")]
    pub confidence: f64,
    /// Whether the user has confirmed this face assignment
    #[sea_orm(default_value = "false")]
    pub is_confirmed: bool,
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
        belongs_to = "super::person::Entity",
        from = "Column::PersonId",
        to = "super::person::Column::Id"
    )]
    Person,
}

impl Related<super::asset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Asset.def()
    }
}

impl Related<super::person::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Person.def()
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
    pub fn find_by_asset_id(asset_id: &str) -> Select<Entity> {
        Self::find().filter(Column::AssetId.eq(asset_id))
    }

    pub fn find_by_person_id(person_id: &str) -> Select<Entity> {
        Self::find().filter(Column::PersonId.eq(person_id))
    }

    pub fn find_unassigned() -> Select<Entity> {
        Self::find().filter(Column::PersonId.is_null())
    }
}
