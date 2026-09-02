pub(crate) mod auth_gate;
pub(crate) mod cache;
pub(crate) mod error;
pub(crate) mod init;
#[allow(clippy::module_inception)]
pub(crate) mod library;
pub(crate) mod lock;
pub(crate) mod open;
pub(crate) mod paths;
pub(crate) mod rebuild;
pub(crate) mod receipts;
pub(crate) mod scrub;
pub(crate) mod space;
pub(crate) mod storage_verify;

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
    ThumbnailSize, media_dir, media_path, meta_cache_path, receipts_path, sidecar_path,
    thumbnail_path, tmp_path, transcode_h264_path, transcode_live_path, trash_path, uuid_shard,
};
pub use rebuild::rebuild_index;
pub use receipts::{
    CustodyReceipt, CustodyReceiptCore, ReceiptExpectations, ReceiptRejection, ReceiptStoreError,
    append_receipt, load_receipts, verify_receipt,
};
pub use space::{available_bytes, largest_asset_fits, streaming_recommended};
pub use storage_verify::{
    BlobRole, BlobVerdict, ReleaseDecision, ReleaseGate, RetainReason, StorageVerdict,
    StorageVerifier, VerifierError, release_is_safe, release_move_source, release_owned_original,
};
