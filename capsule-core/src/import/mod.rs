//! The pure half of import: candidate grouping, special-file classification, the import scope,
//! default-album resolution and the upload policy. Scanning a directory, planning against the
//! index and executing an import are local state and live in `capsule-library`'s `import`
//! module, which re-exports everything here.

pub mod default_album;
pub mod group;
pub mod scan;
pub mod scope;
pub mod special;
pub mod upload;

pub use default_album::{
    DefaultAlbumContext, DefaultAlbumError, ResolutionRule, ResolvedAlbum, resolve_default_album,
};
pub use group::{PRIMARY_EXTS, RAW_EXTS, VIDEO_EXTS, group_by_stem};
pub use scan::{ImportCandidate, ScanResult};
pub use scope::{IMPORT_SCOPE_V1, Scope, SourceKind};
pub use special::{SpecialDirectoryStatus, SpecialFileStatus, SpecialStatus};
pub use upload::{StagedStreamingConflict, UploadPolicy, UploadTier, ensure_streaming_compatible};
