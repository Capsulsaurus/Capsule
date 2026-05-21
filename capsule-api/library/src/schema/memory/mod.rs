use async_graphql::*;
use chrono::{DateTime, Utc};
use entity::memory::Model as MemoryModel;

use super::asset::AssetMetadata;

/// An auto-generated memory (e.g., "On This Day", trip highlights)
pub struct Memory {
    pub model: MemoryModel,
}

#[Object]
impl Memory {
    async fn id(&self) -> ID {
        ID::from(&self.model.id)
    }

    async fn title(&self) -> &String {
        &self.model.title
    }

    async fn subtitle(&self) -> Option<&String> {
        self.model.subtitle.as_ref()
    }

    // TODO: Resolve cover asset via dataloader
    async fn cover_asset(&self) -> Option<AssetMetadata> {
        None
    }

    // TODO: Resolve assets from asset_ids JSON
    async fn assets(&self) -> Vec<AssetMetadata> {
        vec![]
    }

    /// The historical date this memory references
    async fn memory_date(&self) -> DateTime<Utc> {
        self.model.memory_date
    }

    async fn created_at(&self) -> DateTime<Utc> {
        self.model.created_at
    }

    async fn is_seen(&self) -> bool {
        self.model.is_seen
    }
}

// ===== Query =====

#[derive(Default)]
pub struct MemoryQuery;

#[Object]
impl MemoryQuery {
    /// Get today's memories ("On This Day")
    async fn today(&self, _ctx: &Context<'_>) -> Result<Vec<Memory>> {
        todo!("Implement get today's memories")
    }

    /// Get recent unseen memories
    async fn unseen(&self, _ctx: &Context<'_>) -> Result<Vec<Memory>> {
        todo!("Implement get unseen memories")
    }
}

// ===== Mutations =====

#[derive(Default)]
pub struct MemoryMutation;

#[Object]
impl MemoryMutation {
    /// Mark a memory as seen
    async fn mark_seen(&self, _ctx: &Context<'_>, _id: ID) -> Result<bool> {
        todo!("Implement mark memory seen")
    }

    /// Hide a memory (don't show again)
    async fn hide(&self, _ctx: &Context<'_>, _id: ID) -> Result<bool> {
        todo!("Implement hide memory")
    }
}

// TODO: Besides memory, add timeline feature
// timelines can be grouped by date (anniversaries, "x years ago"), highlight detection (best photos from trips/events), event clustering (group of photos by time/location)
