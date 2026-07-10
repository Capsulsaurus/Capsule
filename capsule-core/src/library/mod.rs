pub mod auth_gate;
pub mod cache;
pub mod error;
pub mod init;
#[allow(clippy::module_inception)]
pub mod library;
pub mod lock;
pub mod open;
pub mod paths;
pub mod rebuild;
pub mod receipts;
pub mod scrub;
pub mod space;
pub mod storage_verify;
pub mod trash;

pub use auth_gate::{
    DEFAULT_GRACE, GateError, GateKeeper, GatedQueryError, GatedView, GraceClock, LocalAuthError,
    LocalAuthGate, SystemGraceClock, ViewGuard,
};
pub use cache::{EvictionReport, cache_sweep};
pub use error::LibraryError;
pub use init::init_library;
pub use library::Library;
pub use open::open_library;
pub use paths::{
    ThumbnailSize, media_dir, media_path, meta_cache_path, receipts_path, sidecar_path, tmp_path,
    transcode_h264_path, transcode_live_path, trash_path, uuid_shard,
};
pub use rebuild::rebuild_index;
pub use receipts::{
    CustodyReceipt, CustodyReceiptCore, ReceiptExpectations, ReceiptRejection, append_receipt,
    load_receipts, verify_receipt,
};
pub use space::{available_bytes, streaming_recommended};
pub use storage_verify::{
    BlobRole, BlobVerdict, ReleaseDecision, ReleaseGate, RetainReason, StorageVerdict,
    StorageVerifier, release_is_safe, release_move_source, release_owned_original,
};
