pub(crate) mod default_album;
pub(crate) mod enrichment;
pub(crate) mod executor;
pub(crate) mod executor_cancellation;
pub(crate) mod group;
pub(crate) mod importers;
pub(crate) mod planner;
pub(crate) mod progress;
pub(crate) mod scan;
pub(crate) mod scanner;
pub(crate) mod scope;
pub(crate) mod special;
pub(crate) mod streaming;
pub(crate) mod upload;

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
