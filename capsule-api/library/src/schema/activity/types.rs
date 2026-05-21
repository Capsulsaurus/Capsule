use async_graphql::*;
use serde::{Deserialize, Serialize};

#[derive(Enum, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivityType {
    CreateAlbum,
    DeleteAlbum,
    UpdateAlbum,
    UploadAssets,
    DeleteAsset,
    MoveAsset,
}

// Action enum (could be expanded based on your needs)
#[derive(Enum, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivityAction {
    Created,
    Updated,
    Deleted,
    Shared,
    Moved,
    Uploaded,
}

// Base Activity interface
#[derive(Interface)]
#[graphql(
    field(name = "id", ty = "String"),
    field(name = "type", ty = "ActivityType"),
    field(name = "action", ty = "ActivityAction"),
    field(name = "timestamp", ty = "chrono::DateTime<chrono::Utc>")
)]
pub enum Activity {
    CreateAlbum(CreateAlbumActivity),
    DeleteAlbum(DeleteAlbumActivity),
    UpdateAlbum(UpdateAlbumActivity),
    UploadAssets(UploadAssetsActivity),
    DeleteAsset(DeleteAssetActivity),
    MoveAsset(MoveAssetActivity),
}

// Album Activities
pub struct CreateAlbumActivity {
    pub id: String,
    pub activity_type: ActivityType,
    pub action: ActivityAction,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub album_id: String,
    pub album_name: String,
    pub user_id: String,
}
pub struct DeleteAlbumActivity {
    pub id: String,
    pub activity_type: ActivityType,
    pub action: ActivityAction,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub album_id: String,
    pub album_name: String,
    pub user_id: String,
}

pub struct UpdateAlbumActivity {
    pub id: String,
    pub activity_type: ActivityType,
    pub action: ActivityAction,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub album_id: String,
    pub old_name: Option<String>,
    pub new_name: Option<String>,
    pub user_id: String,
    /// List of changed fields
    pub changes: Vec<String>,
}

// Asset Activities

pub struct UploadAssetsActivity {
    pub id: String,
    pub activity_type: ActivityType,
    pub action: ActivityAction,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub destination_album_id: Option<String>,
    pub destination_album_name: Option<String>,
    pub asset_count: i64,
    pub asset_total_size: i64,
}

pub struct DeleteAssetActivity {
    pub id: String,
    pub activity_type: ActivityType,
    pub action: ActivityAction,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub asset_id: String,
    pub asset_name: String,
    pub source_album_id: Option<String>,
    pub source_album_name: Option<String>,
    pub user_id: String,
}

pub struct MoveAssetActivity {
    pub id: String,
    pub activity_type: ActivityType,
    pub action: ActivityAction,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub asset_id: String,
    pub asset_name: String,
    pub source_album_id: Option<String>,
    pub source_album_name: Option<String>,
    pub target_album_id: Option<String>,
    pub target_album_name: Option<String>,
    pub user_id: String,
}

// Implementation examples for the GraphQL resolver

#[Object]
impl CreateAlbumActivity {
    async fn id(&self) -> &str {
        &self.id
    }

    async fn r#type(&self) -> ActivityType {
        self.activity_type
    }

    async fn action(&self) -> ActivityAction {
        self.action
    }

    async fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }

    async fn album_id(&self) -> &str {
        &self.album_id
    }

    async fn album_name(&self) -> &str {
        &self.album_name
    }

    async fn user_id(&self) -> &str {
        &self.user_id
    }
}

// Implementations for other activity types
#[Object]
impl DeleteAlbumActivity {
    async fn id(&self) -> &str {
        &self.id
    }

    async fn r#type(&self) -> ActivityType {
        self.activity_type
    }

    async fn action(&self) -> ActivityAction {
        self.action
    }

    async fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }

    async fn album_id(&self) -> &str {
        &self.album_id
    }

    async fn album_name(&self) -> &str {
        &self.album_name
    }

    async fn user_id(&self) -> &str {
        &self.user_id
    }
}

#[Object]
impl UpdateAlbumActivity {
    async fn id(&self) -> &str {
        &self.id
    }

    async fn r#type(&self) -> ActivityType {
        self.activity_type
    }

    async fn action(&self) -> ActivityAction {
        self.action
    }

    async fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }

    async fn album_id(&self) -> &str {
        &self.album_id
    }

    async fn old_name(&self) -> &Option<String> {
        &self.old_name
    }

    async fn new_name(&self) -> &Option<String> {
        &self.new_name
    }

    async fn user_id(&self) -> &str {
        &self.user_id
    }

    async fn changes(&self) -> &Vec<String> {
        &self.changes
    }
}

#[Object]
impl UploadAssetsActivity {
    async fn id(&self) -> &str {
        &self.id
    }

    async fn r#type(&self) -> ActivityType {
        self.activity_type
    }

    async fn action(&self) -> ActivityAction {
        self.action
    }

    async fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }
    async fn destination_album_id(&self) -> &Option<String> {
        &self.destination_album_id
    }

    async fn destination_album_name(&self) -> &Option<String> {
        &self.destination_album_name
    }

    async fn asset_count(&self) -> i64 {
        self.asset_count
    }

    async fn asset_total_size(&self) -> i64 {
        self.asset_total_size
    }
}

#[Object]
impl DeleteAssetActivity {
    async fn id(&self) -> &str {
        &self.id
    }

    async fn r#type(&self) -> ActivityType {
        self.activity_type
    }

    async fn action(&self) -> ActivityAction {
        self.action
    }

    async fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }

    async fn asset_id(&self) -> &str {
        &self.asset_id
    }

    async fn asset_name(&self) -> &str {
        &self.asset_name
    }

    async fn source_album_id(&self) -> &Option<String> {
        &self.source_album_id
    }

    async fn source_album_name(&self) -> &Option<String> {
        &self.source_album_name
    }

    async fn user_id(&self) -> &str {
        &self.user_id
    }
}

#[Object]
impl MoveAssetActivity {
    async fn id(&self) -> &str {
        &self.id
    }

    async fn r#type(&self) -> ActivityType {
        self.activity_type
    }

    async fn action(&self) -> ActivityAction {
        self.action
    }

    async fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        self.timestamp
    }

    async fn asset_id(&self) -> &str {
        &self.asset_id
    }

    async fn asset_name(&self) -> &str {
        &self.asset_name
    }

    async fn source_album_id(&self) -> &Option<String> {
        &self.source_album_id
    }

    async fn source_album_name(&self) -> &Option<String> {
        &self.source_album_name
    }

    async fn target_album_id(&self) -> &Option<String> {
        &self.target_album_id
    }

    async fn target_album_name(&self) -> &Option<String> {
        &self.target_album_name
    }

    async fn user_id(&self) -> &str {
        &self.user_id
    }
}
