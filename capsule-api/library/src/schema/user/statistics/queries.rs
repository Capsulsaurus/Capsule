use async_graphql::*;

pub struct UserStatisticsQuery;

#[Object]
impl UserStatisticsQuery {
    /// Total photos
    async fn total_photos(&self) -> i64 {
        11
    }

    /// Total albums
    async fn total_albums(&self) -> i64 {
        12
    }

    /// Storage used in bytes
    async fn used_storage(&self) -> i64 {
        1234567890
    }

    /// Storage used for photos in bytes
    async fn used_storage_photos(&self) -> i64 {
        123234
    }

    /// Storage used for videos in bytes    
    async fn used_storage_videos(&self) -> i64 {
        345456
    }

    /// Storage used for sidecar files in bytes
    async fn used_storage_sidecar(&self) -> i64 {
        456789
    }

    /// Storage used in trash in bytes
    async fn used_storage_trash(&self) -> i64 {
        23456784
    }

    /// Total storage in bytes
    async fn total_storage(&self) -> i64 {
        123456745590
    }

    /// Storage used for similar assets in bytes
    async fn used_storage_similar_assets(&self) -> i64 {
        234567890
    }

    /// Storage used for large files in bytes
    async fn used_storage_large_files(&self) -> i64 {
        123456890
    }

    // TODO: Support querying historical storage usage
}
