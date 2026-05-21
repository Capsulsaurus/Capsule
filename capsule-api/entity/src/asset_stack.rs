use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::{Set, entity::prelude::*};

/// Represents a stack/group of related assets (Burst, Live Photo, HDR, etc.)
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "asset_stacks")]
pub struct Model {
    #[sea_orm(primary_key, column_type = "Char(Some(21))")]
    pub id: String,

    /// Owner of this stack (same as member assets)
    #[sea_orm(indexed)]
    pub owner_id: String,

    /// Type of stack - determines behavior and UI presentation
    pub stack_type: StackType,

    /// The "hero" asset shown when stack is collapsed in grid views
    #[sea_orm(indexed)]
    pub primary_asset_id: String,

    /// Optional separate cover asset (may differ from primary for UI)
    #[sea_orm(nullable)]
    pub cover_asset_id: Option<String>,

    /// Stack-type-specific metadata as JSON (see StackMetadata types in model crate)
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,

    /// Whether stack is collapsed in UI (show only primary asset)
    #[sea_orm(default_value = "true")]
    pub is_collapsed: bool,

    /// Whether stack was auto-detected or manually created by user
    #[sea_orm(default_value = "true")]
    pub is_auto_generated: bool,

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

/// All supported stack types from documentation
/// Using String db_type for maximum extensibility without schema changes
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(20))")]
pub enum StackType {
    // ===== Photography & Mobile =====
    /// RAW + JPEG/HEIC pairs
    #[sea_orm(string_value = "raw_jpeg")]
    RawJpeg,
    /// Burst capture sequence (10-30 fps)
    #[sea_orm(string_value = "burst")]
    Burst,
    /// HEIC/JPEG + short video (Live Photo / Motion Photo)
    #[sea_orm(string_value = "live_photo")]
    LivePhoto,
    /// Image + depth map for adjustable bokeh
    #[sea_orm(string_value = "portrait")]
    Portrait,
    /// AI-grouped visually similar images
    #[sea_orm(string_value = "smart_selection")]
    SmartSelection,

    // ===== Technical & Creative =====
    /// Multiple exposures for HDR (-2, 0, +2 EV)
    #[sea_orm(string_value = "hdr_bracket")]
    HdrBracket,
    /// Shots with shifting focus for infinite DOF
    #[sea_orm(string_value = "focus_stack")]
    FocusStack,
    /// Pixel-shift multi-shot for ultra resolution
    #[sea_orm(string_value = "pixel_shift")]
    PixelShift,
    /// Panorama source sequence
    #[sea_orm(string_value = "panorama")]
    Panorama,

    // ===== Video & Audio =====
    /// Master + Proxy pairs for editing
    #[sea_orm(string_value = "proxy")]
    Proxy,
    /// Chaptered video (GoPro 4GB chunks)
    #[sea_orm(string_value = "chaptered")]
    Chaptered,
    /// Video + external audio sync
    #[sea_orm(string_value = "dual_audio")]
    DualAudio,

    // ===== User-defined =====
    /// Custom user grouping
    #[sea_orm(string_value = "custom")]
    Custom,
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
        from = "Column::PrimaryAssetId",
        to = "super::asset::Column::Id"
    )]
    PrimaryAsset,
    #[sea_orm(
        belongs_to = "super::asset::Entity",
        from = "Column::CoverAssetId",
        to = "super::asset::Column::Id"
    )]
    CoverAsset,
    #[sea_orm(has_many = "super::stack_member::Entity")]
    Members,
}

impl Related<super::owner::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::stack_member::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Members.def()
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

    pub fn find_by_primary_asset_id(asset_id: &str) -> Select<Entity> {
        Self::find().filter(Column::PrimaryAssetId.eq(asset_id))
    }
}
