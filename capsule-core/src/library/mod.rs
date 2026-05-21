pub mod error;
pub mod init;
#[allow(clippy::module_inception)]
pub mod library;
pub mod lock;
pub mod open;
pub mod paths;
pub mod rebuild;
pub mod scrub;
pub mod trash;

pub use error::LibraryError;
pub use init::init_library;
pub use library::Library;
pub use open::open_library;
pub use paths::{
    ThumbnailSize, media_dir, media_path, meta_cache_path, sidecar_path, tmp_path,
    transcode_h264_path, transcode_live_path, trash_path, uuid_shard,
};
pub use rebuild::rebuild_index;
