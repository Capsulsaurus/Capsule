pub(crate) mod asset_sidecar;
pub(crate) mod io;
pub(crate) mod library_config;
pub(crate) mod library_version;
pub(crate) mod shape;
pub(crate) mod sidecar_v1;
pub(crate) mod stack_hint;

pub use asset_sidecar::AssetSidecar;
pub use io::{
    read_library_config, read_library_version, read_sidecar, write_library_config,
    write_library_version,
};
pub use library_config::LibraryConfigCbor;
pub use library_version::LibraryVersionCbor;
pub use sidecar_v1::{
    AiTag, CameraId, CullFlag, Dimensions, Gps, GpsSource, Lqip, SIDECAR_SCHEMA_V1, SidecarV1,
    StackMembership, StackRole,
};
pub use stack_hint::StackHint;
