use async_graphql::*;
use chrono::{DateTime, Utc};
use entity::asset::Model as AssetModel;
use model::asset::AssetType as ModelAssetType;

use crate::schema::Tag;
use crate::schema::stack::AssetStack;

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum AssetType {
    #[graphql(name = "photo")]
    Photo,
    #[graphql(name = "video")]
    Video,
    #[graphql(name = "motion_photo")]
    MotionPhoto,
    #[graphql(name = "sidecar")]
    Sidecar,
}

impl From<ModelAssetType> for AssetType {
    fn from(t: ModelAssetType) -> Self {
        match t {
            ModelAssetType::Photo => AssetType::Photo,
            ModelAssetType::Video => AssetType::Video,
            ModelAssetType::MotionPhoto => AssetType::MotionPhoto,
            ModelAssetType::Sidecar => AssetType::Sidecar,
        }
    }
}

/// GPS location with optional altitude and reverse-geocoded name
#[derive(SimpleObject)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    /// Reverse-geocoded place name (e.g., "San Francisco, CA")
    pub name: Option<String>,
}

/// EXIF/technical metadata from camera
#[derive(SimpleObject)]
pub struct ExifData {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub iso: Option<i32>,
    /// Aperture f-number (e.g., 2.8)
    pub aperture: Option<f64>,
    /// Exposure time for display (e.g., "1/500")
    pub exposure_time: Option<String>,
    /// Exposure time in milliseconds for sorting/filtering
    pub exposure_time_ms: Option<f64>,
    pub flash_fired: Option<bool>,
    /// EXIF orientation value (1-8)
    pub orientation: Option<i32>,
}

/// Face bounding box (normalized 0-1 coordinates)
#[derive(SimpleObject, serde::Deserialize)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Input for GPS coordinates
#[derive(InputObject)]
pub struct GPSCoordinatesInput {
    pub latitude: f64,
    pub longitude: f64,
}

/// Input for bounding box geographic filter
#[derive(InputObject)]
pub struct BoundingBoxInput {
    pub north_east: GPSCoordinatesInput,
    pub south_west: GPSCoordinatesInput,
}

pub struct AssetMetadata {
    pub model: AssetModel,
}

#[Object]
impl AssetMetadata {
    async fn id(&self) -> ID {
        ID::from(&self.model.id)
    }

    #[graphql(name = "type")]
    #[allow(unreachable_code, clippy::diverging_sub_expression)]
    async fn asset_type(&self) -> AssetType {
        // self.model.asset_type.into()
        todo!("Implement asset type handling")
    }

    async fn file_name(&self) -> &String {
        &self.model.original_filename
    }

    /// Size of the asset in bytes
    async fn size(&self) -> i64 {
        self.model.file_size
    }

    async fn content_type(&self) -> &String {
        &self.model.content_type
    }

    async fn url(&self) -> String {
        // TODO: Generate signed URL or path
        format!("/v1/media/{}", self.model.id)
    }

    async fn width(&self) -> i32 {
        self.model.width
    }

    async fn height(&self) -> i32 {
        self.model.height
    }

    // ===== New fields from schema freeze =====

    /// GPS location if available
    async fn location(&self) -> Option<Location> {
        match (self.model.latitude, self.model.longitude) {
            (Some(lat), Some(lng)) => Some(Location {
                latitude: lat,
                longitude: lng,
                altitude: None, // Stored in external metadata
                name: None,     // Resolved via reverse geocoding service
            }),
            _ => None,
        }
    }

    /// Low Quality Image Placeholder hash for instant loading
    async fn lqip_hash(&self) -> Option<&String> {
        self.model.lqip_hash.as_ref()
    }

    /// Dominant color hex code (e.g., "#FF5733")
    async fn dominant_color(&self) -> Option<&String> {
        self.model.dominant_color.as_ref()
    }

    /// Whether this asset is marked as favorite
    async fn is_favorite(&self) -> bool {
        self.model.is_favorite
    }

    /// EXIF/technical metadata (resolved from external metadata store)
    async fn exif(&self) -> Option<ExifData> {
        // TODO: Resolve from external metadata store
        None
    }

    // ===== Stack Membership =====

    /// If this asset belongs to a stack, return the stack
    async fn stack(&self, ctx: &Context<'_>) -> Result<Option<AssetStack>> {
        if let Some(stack_id) = &self.model.stack_id {
            // TODO: Use Dataloader for efficiency
            let db = ctx.data::<sea_orm::DatabaseConnection>()?;
            let stack = service::stack::Query::find_by_id(db, stack_id).await?;
            Ok(stack.map(|model| AssetStack { model }))
        } else {
            Ok(None)
        }
    }

    /// Whether this asset should be hidden in main grid (because it's buried in a stack)
    async fn is_stack_hidden(&self) -> bool {
        self.model.is_stack_hidden
    }

    // ===== Timestamps (renamed date → capturedAt) =====

    /// When the asset was captured/taken (from EXIF DateTimeOriginal)
    async fn captured_at(&self) -> Option<DateTime<Utc>> {
        self.model.captured_at.map(|d| d.with_timezone(&Utc))
    }

    async fn uploaded_at(&self) -> DateTime<Utc> {
        self.model.uploaded_at.with_timezone(&Utc)
    }

    async fn modified_at(&self) -> DateTime<Utc> {
        self.model.modified_at.with_timezone(&Utc)
    }

    async fn deleted_at(&self) -> Option<DateTime<Utc>> {
        self.model.deleted_at.map(|d| d.with_timezone(&Utc))
    }

    // TODO: Implement Tag resolution using Dataloader
    async fn tags(&self) -> Vec<Tag> {
        vec![]
    }

    // TODO: Implement faces resolution
    // async fn faces(&self) -> Vec<Face> { vec![] }
}

#[derive(InputObject)]
pub struct CreateAssetInput {
    /// ID of the upload session
    pub session_id: ID,
    /// ID of the album to add the asset to
    pub album_id: Option<ID>,
    /// Optional description (if we add it to DB later)
    pub description: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateAssetInput {
    pub id: ID,
    pub description: Option<String>,
    /// When the asset was captured (EXIF date)
    pub captured_at: Option<DateTime<Utc>>,
    /// Set favorite status
    pub is_favorite: Option<bool>,
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum ImageFormat {
    Jpeg,
    Webp,
    Avif,
}

#[derive(InputObject)]
pub struct AssetFilter {
    pub album_id: Option<ID>,
    pub upload_session_id: Option<ID>,
    /// Filter by geographic bounding box
    pub bounding_box: Option<BoundingBoxInput>,
    /// Filter to only assets with GPS location
    pub has_location: Option<bool>,
    /// Filter by favorite status
    pub is_favorite: Option<bool>,
    /// Filter by person IDs (assets containing these people)
    pub person_ids: Option<Vec<ID>>,
    /// Filter by smart tag names
    pub smart_tags: Option<Vec<String>>,
    /// Filter by media types
    pub media_types: Option<Vec<AssetType>>,
}

/// Date range filter
#[derive(InputObject)]
pub struct DateRangeInput {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

/// Geographic radius filter
#[derive(InputObject)]
pub struct GeoRadiusInput {
    pub center: GPSCoordinatesInput,
    pub radius_km: f64,
}

/// Smart search input with natural language and structured filters
#[derive(InputObject)]
pub struct SearchInput {
    /// Natural language query (e.g., "photos of dogs at the beach")
    pub query: Option<String>,
    /// Date range filter
    pub date_range: Option<DateRangeInput>,
    /// Geographic radius filter
    pub geo_radius: Option<GeoRadiusInput>,
    /// Filter by person IDs
    pub person_ids: Option<Vec<ID>>,
    /// Filter by album IDs
    pub album_ids: Option<Vec<ID>>,
    /// Filter by smart tags
    pub smart_tags: Option<Vec<String>>,
    /// Filter by media types
    pub media_types: Option<Vec<AssetType>>,
    /// Filter by favorite status
    pub is_favorite: Option<bool>,
    /// Pagination limit
    pub limit: Option<i32>,
    /// Pagination cursor
    pub cursor: Option<String>,
}

#[derive(InputObject)]
pub struct AssetSort {
    pub direction: crate::schema::SortDirection,
    pub field: AssetSortField,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum AssetSortField {
    /// Sort by capture date (when the photo was taken)
    CapturedAt,
    /// Sort by upload date
    UploadedAt,
    /// Sort by file size
    FileSize,
}

#[derive(SimpleObject)]
pub struct UploadSession {
    pub id: ID,
    pub created_at: DateTime<Utc>,
    pub status: String,
}

#[derive(InputObject)]
pub struct UploadSessionFilter {
    pub status: Option<String>,
}
