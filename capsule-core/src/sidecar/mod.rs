pub(crate) mod album_doc;
pub(crate) mod export;
pub(crate) mod label;
pub(crate) mod library_config;
pub mod library_version;
pub(crate) mod region;
pub mod shape;
pub mod sidecar_v1;
pub(crate) mod signed_doc;
pub(crate) mod stack_doc;

pub use album_doc::{ALBUM_DOC_SCHEMA_V1, AlbumDocV1};
pub use export::{ExportPrivacy, sidecar_for_export};
pub use label::{LabelError, MAX_LABEL_BYTES, normalize_label};
pub use library_config::LibraryConfigCbor;
pub use library_version::LibraryVersionCbor;
pub use region::{
    Region, RegionError, RegionShape, Regions, live_region, merge_region_register, merge_regions,
    write_region,
};
pub use sidecar_v1::{
    AiTag, CameraId, CullFlag, Dimensions, Gps, GpsSource, Lqip, SIDECAR_SCHEMA_V1, SidecarV1,
    StackMembership, StackRole,
};
pub use signed_doc::DocError;
pub use stack_doc::{STACK_DOC_SCHEMA_V1, StackDocV1};
