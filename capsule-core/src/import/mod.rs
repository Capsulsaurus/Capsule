pub mod default_album;
pub mod enrichment;
pub mod executor;
pub mod executor_cancellation;
pub mod group;
pub mod importers;
pub mod planner;
pub mod progress;
pub mod scan;
pub mod scanner;
pub mod scope;
pub mod special;
pub mod streaming;
pub mod upload;

pub use default_album::{
    DefaultAlbumContext, DefaultAlbumError, ResolutionRule, ResolvedAlbum, resolve_default_album,
};
pub use enrichment::{CAPTION_MAX_BYTES, FAVORITE_RATING, SourceMetadataIndex, sidecar_enrichment};
pub use executor::{execute, execute_with_source_metadata};
pub use executor_cancellation::CancellationToken;
pub use group::{PRIMARY_EXTS, RAW_EXTS, VIDEO_EXTS, group_by_stem, is_supported_extension};
pub use importers::takeout::TakeoutAdapter;
pub use importers::{
    AdapterError, ExtractedImport, ExtractedMetadata, FoldSource, GeoPoint, ImportProvider,
    SourceAdapter, SourceEntry,
};
pub use planner::{ImportActionPlan, ImportConfig, ImportDecision, PlanCounts, plan};
pub use progress::{ImportExecutionSummary, ImportOutcome, ImportProgressEvent};
pub use scan::{ImportCandidate, ScanResult};
pub use scanner::scan as scan_paths;
pub use scope::{IMPORT_SCOPE_V1, Scope, SourceKind};
pub use special::{SpecialDirectoryStatus, SpecialFileStatus, SpecialStatus};
pub use streaming::{
    AssetUploader, StreamHalt, StreamedOutcome, StreamedState, StreamingError, StreamingEvent,
    StreamingReport, UploadHalt, execute_streaming, execute_streaming_with_source_metadata,
};
pub use upload::{StagedStreamingConflict, UploadPolicy, UploadTier, ensure_streaming_compatible};
