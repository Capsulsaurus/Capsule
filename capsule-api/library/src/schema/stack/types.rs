use async_graphql::*;
use chrono::{DateTime, Utc};
use entity::{asset_stack::Model as StackModel, stack_member::Model as MemberModel};
use model;

use crate::schema::asset::AssetMetadata;

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum StackType {
    #[graphql(name = "raw_jpeg")]
    RawJpeg,
    #[graphql(name = "burst")]
    Burst,
    #[graphql(name = "live_photo")]
    LivePhoto,
    #[graphql(name = "portrait")]
    Portrait,
    #[graphql(name = "smart_selection")]
    SmartSelection,
    #[graphql(name = "hdr_bracket")]
    HdrBracket,
    #[graphql(name = "focus_stack")]
    FocusStack,
    #[graphql(name = "pixel_shift")]
    PixelShift,
    #[graphql(name = "panorama")]
    Panorama,
    #[graphql(name = "proxy")]
    Proxy,
    #[graphql(name = "chaptered")]
    Chaptered,
    #[graphql(name = "dual_audio")]
    DualAudio,
    #[graphql(name = "custom")]
    Custom,
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum MemberRole {
    #[graphql(name = "primary")]
    Primary,
    #[graphql(name = "video")]
    Video,
    #[graphql(name = "audio")]
    Audio,
    #[graphql(name = "depth_map")]
    DepthMap,
    #[graphql(name = "processed")]
    Processed,
    #[graphql(name = "raw")]
    Raw,
    #[graphql(name = "source")]
    Source,
    #[graphql(name = "alternate")]
    Alternate,
    #[graphql(name = "sidecar")]
    Sidecar,
    #[graphql(name = "proxy")]
    Proxy,
    #[graphql(name = "master")]
    Master,
}

// ===== From model layer (NOT entity layer) for proper layer separation =====

impl From<model::stack::StackType> for StackType {
    fn from(t: model::stack::StackType) -> Self {
        match t {
            model::stack::StackType::RawJpeg => StackType::RawJpeg,
            model::stack::StackType::Burst => StackType::Burst,
            model::stack::StackType::LivePhoto => StackType::LivePhoto,
            model::stack::StackType::Portrait => StackType::Portrait,
            model::stack::StackType::SmartSelection => StackType::SmartSelection,
            model::stack::StackType::HdrBracket => StackType::HdrBracket,
            model::stack::StackType::FocusStack => StackType::FocusStack,
            model::stack::StackType::PixelShift => StackType::PixelShift,
            model::stack::StackType::Panorama => StackType::Panorama,
            model::stack::StackType::Proxy => StackType::Proxy,
            model::stack::StackType::Chaptered => StackType::Chaptered,
            model::stack::StackType::DualAudio => StackType::DualAudio,
            model::stack::StackType::Custom => StackType::Custom,
        }
    }
}

impl From<model::stack::MemberRole> for MemberRole {
    fn from(r: model::stack::MemberRole) -> Self {
        match r {
            model::stack::MemberRole::Primary => MemberRole::Primary,
            model::stack::MemberRole::Video => MemberRole::Video,
            model::stack::MemberRole::Audio => MemberRole::Audio,
            model::stack::MemberRole::DepthMap => MemberRole::DepthMap,
            model::stack::MemberRole::Processed => MemberRole::Processed,
            model::stack::MemberRole::Raw => MemberRole::Raw,
            model::stack::MemberRole::Source => MemberRole::Source,
            model::stack::MemberRole::Alternate => MemberRole::Alternate,
            model::stack::MemberRole::Sidecar => MemberRole::Sidecar,
            model::stack::MemberRole::Proxy => MemberRole::Proxy,
            model::stack::MemberRole::Master => MemberRole::Master,
        }
    }
}

// ===== Reverse: GraphQL input → model layer for mutations =====

impl From<StackType> for model::stack::StackType {
    fn from(t: StackType) -> Self {
        match t {
            StackType::RawJpeg => model::stack::StackType::RawJpeg,
            StackType::Burst => model::stack::StackType::Burst,
            StackType::LivePhoto => model::stack::StackType::LivePhoto,
            StackType::Portrait => model::stack::StackType::Portrait,
            StackType::SmartSelection => model::stack::StackType::SmartSelection,
            StackType::HdrBracket => model::stack::StackType::HdrBracket,
            StackType::FocusStack => model::stack::StackType::FocusStack,
            StackType::PixelShift => model::stack::StackType::PixelShift,
            StackType::Panorama => model::stack::StackType::Panorama,
            StackType::Proxy => model::stack::StackType::Proxy,
            StackType::Chaptered => model::stack::StackType::Chaptered,
            StackType::DualAudio => model::stack::StackType::DualAudio,
            StackType::Custom => model::stack::StackType::Custom,
        }
    }
}

impl From<MemberRole> for model::stack::MemberRole {
    fn from(r: MemberRole) -> Self {
        match r {
            MemberRole::Primary => model::stack::MemberRole::Primary,
            MemberRole::Video => model::stack::MemberRole::Video,
            MemberRole::Audio => model::stack::MemberRole::Audio,
            MemberRole::DepthMap => model::stack::MemberRole::DepthMap,
            MemberRole::Processed => model::stack::MemberRole::Processed,
            MemberRole::Raw => model::stack::MemberRole::Raw,
            MemberRole::Source => model::stack::MemberRole::Source,
            MemberRole::Alternate => model::stack::MemberRole::Alternate,
            MemberRole::Sidecar => model::stack::MemberRole::Sidecar,
            MemberRole::Proxy => model::stack::MemberRole::Proxy,
            MemberRole::Master => model::stack::MemberRole::Master,
        }
    }
}

pub struct AssetStack {
    pub model: StackModel,
}

#[Object]
impl AssetStack {
    async fn id(&self) -> ID {
        ID::from(&self.model.id)
    }

    async fn stack_type(&self) -> StackType {
        // Entity enum -> Model enum -> GraphQL enum
        // We go through model layer as planned
        let model_type: model::stack::StackType = self.model.stack_type.clone().into();
        model_type.into()
    }

    async fn is_collapsed(&self) -> bool {
        self.model.is_collapsed
    }

    async fn is_auto_generated(&self) -> bool {
        self.model.is_auto_generated
    }

    async fn metadata(&self) -> Option<String> {
        self.model.metadata.as_ref().map(|v| v.to_string())
    }

    async fn created_at(&self) -> DateTime<Utc> {
        self.model.created_at
    }

    async fn modified_at(&self) -> DateTime<Utc> {
        self.model.modified_at
    }

    /// Primary asset for this stack
    async fn primary_asset(&self, _ctx: &Context<'_>) -> Result<AssetMetadata> {
        todo!("Resolve primary asset via dataloader")
    }

    /// Cover asset (may differ from primary)
    async fn cover_asset(&self, _ctx: &Context<'_>) -> Result<Option<AssetMetadata>> {
        todo!("Resolve cover asset via dataloader")
    }

    /// All members of this stack
    async fn members(&self, _ctx: &Context<'_>) -> Result<Vec<StackMember>> {
        todo!("Resolve members via service")
    }

    /// Count of members in this stack
    async fn member_count(&self, _ctx: &Context<'_>) -> Result<i32> {
        todo!("Count members")
    }
}

pub struct StackMember {
    pub model: MemberModel,
}

#[Object]
impl StackMember {
    async fn id(&self) -> ID {
        ID::from(&self.model.id)
    }

    async fn sequence_order(&self) -> i32 {
        self.model.sequence_order
    }

    async fn role(&self) -> MemberRole {
        let model_role: model::stack::MemberRole = self.model.member_role.clone().into();
        model_role.into()
    }

    async fn metadata(&self) -> Option<String> {
        self.model.metadata.as_ref().map(|v| v.to_string())
    }

    async fn created_at(&self) -> DateTime<Utc> {
        self.model.created_at
    }

    /// The asset in this membership
    async fn asset(&self, _ctx: &Context<'_>) -> Result<AssetMetadata> {
        todo!("Resolve asset via dataloader")
    }
}

#[derive(InputObject)]
pub struct CreateStackInput {
    pub stack_type: StackType,
    pub primary_asset_id: ID,
    pub cover_asset_id: Option<ID>,
    pub metadata: Option<String>,
    pub member_ids: Vec<ID>,
}

#[derive(InputObject)]
pub struct AddStackMemberInput {
    pub stack_id: ID,
    pub asset_id: ID,
    pub role: MemberRole,
    pub metadata: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateStackInput {
    pub id: ID,
    pub primary_asset_id: Option<ID>,
    pub cover_asset_id: Option<ID>,
    pub is_collapsed: Option<bool>,
    pub metadata: Option<String>,
}
