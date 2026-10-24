//! Import: scanning a source, planning against the library index, and executing the plan
//! (staged or streaming). The pure half — grouping, scope, default-album resolution and the
//! upload policy — is `capsule_core::import`, re-exported here so callers name one path.

pub(crate) mod camera;
pub(crate) mod enrichment;
pub(crate) mod executor;
pub(crate) mod executor_cancellation;
pub(crate) mod importers;
pub(crate) mod planner;
pub(crate) mod progress;
pub(crate) mod scanner;
pub(crate) mod streaming;

pub use camera::{
    CameraAdapter, CameraDevice, CameraError, CameraObject, CameraReceipt, ObjectFailure,
    PtpCamera, ReceivedObject,
};
pub use capsule_core::import::*;
pub use enrichment::{CAPTION_MAX_BYTES, FAVORITE_RATING, SourceMetadataIndex, sidecar_enrichment};
pub use executor::{execute, execute_with_source_metadata};
pub use executor_cancellation::CancellationToken;
pub use importers::takeout::TakeoutAdapter;
pub use importers::{
    AdapterError, ExtractedImport, ExtractedMetadata, FoldSource, GeoPoint, ImportProvider,
    SourceAdapter, SourceEntry,
};
pub use planner::{ImportActionPlan, ImportConfig, ImportDecision, PlanCounts, plan};
pub use progress::{ImportExecutionSummary, ImportOutcome, ImportProgressEvent};
pub use scanner::scan as scan_paths;
pub use streaming::{
    AssetUploader, StreamHalt, StreamedOutcome, StreamedState, StreamingError, StreamingEvent,
    StreamingOptions, StreamingReport, UploadHalt, execute_streaming,
};
