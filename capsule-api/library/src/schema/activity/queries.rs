use super::{
    Activity, ActivityAction, ActivityType, CreateAlbumActivity, DeleteAlbumActivity,
    DeleteAssetActivity, MoveAssetActivity, UpdateAlbumActivity, UploadAssetsActivity,
};
use async_graphql::*;

pub struct ActivityQuery;

#[Object]
impl ActivityQuery {
    /// Get activity by ID
    async fn by_id(&self, _ctx: &Context<'_>, _id: ID) -> Result<Activity> {
        todo!()
    }

    /// Search activities based on filter, and pagination
    async fn search(&self, _ctx: &Context<'_>) -> Result<Vec<Activity>> {
        // TODO: Add filter, sorting and pagination
        // todo!()
        // TODO: Remove hardcode
        Ok(vec![
            Activity::CreateAlbum(CreateAlbumActivity {
                id: "123".to_string(),
                activity_type: ActivityType::CreateAlbum,
                action: ActivityAction::Created,
                timestamp: chrono::Utc::now(),
                album_id: "123".to_string(),
                album_name: "Test Album".to_string(),
                user_id: "123".to_string(),
            }),
            Activity::DeleteAlbum(DeleteAlbumActivity {
                id: "456".to_string(),
                activity_type: ActivityType::DeleteAlbum,
                action: ActivityAction::Deleted,
                timestamp: chrono::Utc::now(),
                album_id: "123".to_string(),
                album_name: "Test Album".to_string(),
                user_id: "123".to_string(),
            }),
            Activity::UpdateAlbum(UpdateAlbumActivity {
                id: "789".to_string(),
                activity_type: ActivityType::UpdateAlbum,
                action: ActivityAction::Updated,
                timestamp: chrono::Utc::now(),
                album_id: "123".to_string(),
                old_name: Some("Old Name".to_string()),
                new_name: Some("New Name".to_string()),
                user_id: "123".to_string(),
                changes: vec![],
            }),
            Activity::UploadAssets(UploadAssetsActivity {
                id: "102".to_string(),
                activity_type: ActivityType::UploadAssets,
                action: ActivityAction::Uploaded,
                timestamp: chrono::Utc::now(),
                destination_album_id: Some("123".to_string()),
                destination_album_name: Some("Test Album".to_string()),
                asset_count: 3,
                asset_total_size: 234456,
            }),
            Activity::DeleteAsset(DeleteAssetActivity {
                id: "103".to_string(),
                activity_type: ActivityType::DeleteAsset,
                action: ActivityAction::Deleted,
                timestamp: chrono::Utc::now(),
                asset_id: "123".to_string(),
                asset_name: "Test Asset".to_string(),
                source_album_id: Some("123".to_string()),
                source_album_name: Some("Test Album".to_string()),
                user_id: "123".to_string(),
            }),
            Activity::MoveAsset(MoveAssetActivity {
                id: "104".to_string(),
                activity_type: ActivityType::MoveAsset,
                action: ActivityAction::Moved,
                timestamp: chrono::Utc::now(),
                asset_id: "123".to_string(),
                asset_name: "Test Asset".to_string(),
                source_album_id: Some("123".to_string()),
                source_album_name: Some("Test Album".to_string()),
                target_album_id: Some("456".to_string()),
                target_album_name: Some("Test Album".to_string()),
                user_id: "123".to_string(),
            }),
        ])
    }
}
