use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{Set, entity::prelude::*};

/// Represents a recognized person across the photo library
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "people")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,
    /// Owner of this person entry (user who owns the library)
    #[sea_orm(indexed)]
    pub owner_id: String,
    /// User-assigned name for the person (e.g., "Mom", "John")
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub name: Option<String>,
    /// ID of the asset used as the cover/representative photo
    #[sea_orm(nullable)]
    pub cover_photo_id: Option<String>,
    /// Whether this person is hidden from UI
    #[sea_orm(default_value = "false")]
    pub is_hidden: bool,
    /// Cached count of faces assigned to this person
    #[sea_orm(default_value = "0")]
    pub face_count: i32,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP",
        on_update = "CURRENT_TIMESTAMP"
    )]
    pub modified_at: DateTime<Utc>,
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
        from = "Column::CoverPhotoId",
        to = "super::asset::Column::Id"
    )]
    CoverPhoto,
    #[sea_orm(has_many = "super::face::Entity")]
    Faces,
}

impl Related<super::owner::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::asset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CoverPhoto.def()
    }
}

impl Related<super::face::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Faces.def()
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

    pub fn find_visible_by_owner_id(owner_id: &str) -> Select<Entity> {
        Self::find()
            .filter(Column::OwnerId.eq(owner_id))
            .filter(Column::IsHidden.eq(false))
    }
}
