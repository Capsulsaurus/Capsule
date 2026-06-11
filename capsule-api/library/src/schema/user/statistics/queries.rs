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
        1_234_567_890
    }

    /// Storage used for photos in bytes
    async fn used_storage_photos(&self) -> i64 {
        123_234
    }

    /// Storage used for videos in bytes    
    async fn used_storage_videos(&self) -> i64 {
        345_456
    }

    /// Storage used for sidecar files in bytes
    async fn used_storage_sidecar(&self) -> i64 {
        456_789
    }

    /// Storage used in trash in bytes
    async fn used_storage_trash(&self) -> i64 {
        23_456_784
    }

    /// Total storage in bytes
    async fn total_storage(&self) -> i64 {
        123_456_745_590
    }

    /// Storage used for similar assets in bytes
    async fn used_storage_similar_assets(&self) -> i64 {
        234_567_890
    }

    /// Storage used for large files in bytes
    async fn used_storage_large_files(&self) -> i64 {
        123_456_890
    }

    // TODO: Support querying historical storage usage
}
