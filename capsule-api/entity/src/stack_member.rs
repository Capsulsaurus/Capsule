use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{QueryOrder, Set, entity::prelude::*};

/// Links assets to stacks with ordering and role information
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "stack_members")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,

    /// Parent stack
    #[sea_orm(indexed)]
    pub stack_id: String,

    /// Member asset
    #[sea_orm(indexed)]
    pub asset_id: String,

    /// Position in sequence (0-indexed)
    /// Burst: frame order | HDR: EV order | Live Photo: 0=still, 1=video
    pub sequence_order: i32,

    /// Role of this asset within the stack
    pub member_role: MemberRole,

    /// Role-specific metadata as JSON
    /// HDR: {"ev_value": -2.0} | Portrait: {"blur_strength": 0.8}
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,

    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

/// Role of an asset within a stack
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum MemberRole {
    /// Primary/hero asset shown when collapsed
    #[sea_orm(string_value = "primary")]
    Primary,
    /// Video component (Live Photo)
    #[sea_orm(string_value = "video")]
    Video,
    /// Audio component (Dual-System Audio)
    #[sea_orm(string_value = "audio")]
    Audio,
    /// Depth map data (Portrait)
    #[sea_orm(string_value = "depth_map")]
    DepthMap,
    /// Processed/rendered output (HDR merge, bokeh-applied)
    #[sea_orm(string_value = "processed")]
    Processed,
    /// Unprocessed RAW file
    #[sea_orm(string_value = "raw")]
    Raw,
    /// Original source frame
    #[sea_orm(string_value = "source")]
    Source,
    /// Alternative/hidden shot (Burst, Smart Selection)
    #[sea_orm(string_value = "alternate")]
    Alternate,
    /// Sidecar/XMP metadata file
    #[sea_orm(string_value = "sidecar")]
    Sidecar,
    /// Proxy/optimized version for editing
    #[sea_orm(string_value = "proxy")]
    Proxy,
    /// Master/original high-quality file
    #[sea_orm(string_value = "master")]
    Master,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::asset_stack::Entity",
        from = "Column::StackId",
        to = "super::asset_stack::Column::Id"
    )]
    Stack,
    #[sea_orm(
        belongs_to = "super::asset::Entity",
        from = "Column::AssetId",
        to = "super::asset::Column::Id"
    )]
    Asset,
}

impl Related<super::asset_stack::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Stack.def()
    }
}

impl Related<super::asset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Asset.def()
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
    pub fn find_by_stack_id(stack_id: &str) -> Select<Entity> {
        Self::find()
            .filter(Column::StackId.eq(stack_id))
            .order_by_asc(Column::SequenceOrder)
    }

    pub fn find_by_asset_id(asset_id: &str) -> Select<Entity> {
        Self::find().filter(Column::AssetId.eq(asset_id))
    }
}
