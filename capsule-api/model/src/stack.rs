use serde::{Deserialize, Serialize};

/// Stack type enum mirroring entity layer for API use
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackType {
    RawJpeg,
    Burst,
    LivePhoto,
    Portrait,
    SmartSelection,
    HdrBracket,
    FocusStack,
    PixelShift,
    Panorama,
    Proxy,
    Chaptered,
    DualAudio,
    Custom,
}

impl From<entity::asset_stack::StackType> for StackType {
    fn from(t: entity::asset_stack::StackType) -> Self {
        match t {
            entity::asset_stack::StackType::RawJpeg => StackType::RawJpeg,
            entity::asset_stack::StackType::Burst => StackType::Burst,
            entity::asset_stack::StackType::LivePhoto => StackType::LivePhoto,
            entity::asset_stack::StackType::Portrait => StackType::Portrait,
            entity::asset_stack::StackType::SmartSelection => StackType::SmartSelection,
            entity::asset_stack::StackType::HdrBracket => StackType::HdrBracket,
            entity::asset_stack::StackType::FocusStack => StackType::FocusStack,
            entity::asset_stack::StackType::PixelShift => StackType::PixelShift,
            entity::asset_stack::StackType::Panorama => StackType::Panorama,
            entity::asset_stack::StackType::Proxy => StackType::Proxy,
            entity::asset_stack::StackType::Chaptered => StackType::Chaptered,
            entity::asset_stack::StackType::DualAudio => StackType::DualAudio,
            entity::asset_stack::StackType::Custom => StackType::Custom,
        }
    }
}

impl From<StackType> for entity::asset_stack::StackType {
    fn from(t: StackType) -> Self {
        match t {
            StackType::RawJpeg => entity::asset_stack::StackType::RawJpeg,
            StackType::Burst => entity::asset_stack::StackType::Burst,
            StackType::LivePhoto => entity::asset_stack::StackType::LivePhoto,
            StackType::Portrait => entity::asset_stack::StackType::Portrait,
            StackType::SmartSelection => entity::asset_stack::StackType::SmartSelection,
            StackType::HdrBracket => entity::asset_stack::StackType::HdrBracket,
            StackType::FocusStack => entity::asset_stack::StackType::FocusStack,
            StackType::PixelShift => entity::asset_stack::StackType::PixelShift,
            StackType::Panorama => entity::asset_stack::StackType::Panorama,
            StackType::Proxy => entity::asset_stack::StackType::Proxy,
            StackType::Chaptered => entity::asset_stack::StackType::Chaptered,
            StackType::DualAudio => entity::asset_stack::StackType::DualAudio,
            StackType::Custom => entity::asset_stack::StackType::Custom,
        }
    }
}

/// Member role enum mirroring entity layer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Primary,
    Video,
    Audio,
    DepthMap,
    Processed,
    Raw,
    Source,
    Alternate,
    Sidecar,
    Proxy,
    Master,
}

impl From<entity::stack_member::MemberRole> for MemberRole {
    fn from(r: entity::stack_member::MemberRole) -> Self {
        match r {
            entity::stack_member::MemberRole::Primary => MemberRole::Primary,
            entity::stack_member::MemberRole::Video => MemberRole::Video,
            entity::stack_member::MemberRole::Audio => MemberRole::Audio,
            entity::stack_member::MemberRole::DepthMap => MemberRole::DepthMap,
            entity::stack_member::MemberRole::Processed => MemberRole::Processed,
            entity::stack_member::MemberRole::Raw => MemberRole::Raw,
            entity::stack_member::MemberRole::Source => MemberRole::Source,
            entity::stack_member::MemberRole::Alternate => MemberRole::Alternate,
            entity::stack_member::MemberRole::Sidecar => MemberRole::Sidecar,
            entity::stack_member::MemberRole::Proxy => MemberRole::Proxy,
            entity::stack_member::MemberRole::Master => MemberRole::Master,
        }
    }
}

impl From<MemberRole> for entity::stack_member::MemberRole {
    fn from(r: MemberRole) -> Self {
        match r {
            MemberRole::Primary => entity::stack_member::MemberRole::Primary,
            MemberRole::Video => entity::stack_member::MemberRole::Video,
            MemberRole::Audio => entity::stack_member::MemberRole::Audio,
            MemberRole::DepthMap => entity::stack_member::MemberRole::DepthMap,
            MemberRole::Processed => entity::stack_member::MemberRole::Processed,
            MemberRole::Raw => entity::stack_member::MemberRole::Raw,
            MemberRole::Source => entity::stack_member::MemberRole::Source,
            MemberRole::Alternate => entity::stack_member::MemberRole::Alternate,
            MemberRole::Sidecar => entity::stack_member::MemberRole::Sidecar,
            MemberRole::Proxy => entity::stack_member::MemberRole::Proxy,
            MemberRole::Master => entity::stack_member::MemberRole::Master,
        }
    }
}

/// Stack-type-specific metadata (stored as JSON in DB)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StackMetadata {
    Burst {
        fps: Option<f32>,
        duration_ms: Option<u32>,
        selection_method: Option<String>, // "auto", "manual"
    },
    LivePhoto {
        video_duration_ms: Option<u32>,
        audio_enabled: Option<bool>,
    },
    Portrait {
        depth_format: Option<String>, // "disparity", "depth"
        aperture_range: Option<(f32, f32)>,
    },
    HdrBracket {
        ev_values: Vec<f32>, // e.g., [-2.0, 0.0, 2.0]
        merge_algorithm: Option<String>,
    },
    FocusStack {
        focus_count: Option<u32>,
    },
    Panorama {
        direction: Option<String>, // "horizontal", "vertical"
        overlap_percent: Option<u32>,
    },
    Chaptered {
        total_duration_ms: Option<u64>,
        chunk_count: u32,
    },
    Proxy {
        master_resolution: Option<String>, // "8K", "4K"
        proxy_resolution: Option<String>,
    },
    Default {},
}

/// Input for creating a stack
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateStack {
    pub owner_id: String,
    pub stack_type: StackType,
    pub primary_asset_id: String,
    pub cover_asset_id: Option<String>,
    pub metadata: Option<StackMetadata>,
    pub asset_ids: Vec<String>,
}

/// Input for adding member to stack
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddStackMember {
    pub stack_id: String,
    pub asset_id: String,
    pub sequence_order: i32,
    pub member_role: MemberRole,
    pub metadata: Option<serde_json::Value>,
}
