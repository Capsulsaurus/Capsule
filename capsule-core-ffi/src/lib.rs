//! UniFFI bindings exposing the `capsule-core` SQLite catalog and CBOR sidecar
//! to Swift (and, in future, other UniFFI targets such as Android/Kotlin).
//!
//! This is the **only** FFI-aware crate in the workspace. `capsule-core` stays a
//! pure, portable library; everything platform-specific (filesystem layout,
//! file I/O, PhotoKit, hashing) lives in the Swift client. The types defined
//! here form the explicit Rust ↔ Swift contract:
//!
//! - [`Catalog`] — a thread-safe handle over the SQLite catalog.
//! - [`AssetRecord`], [`AssetStackRecord`], [`StackMemberRecord`],
//!   [`AlbumRecord`] — catalog row mirrors.
//! - [`AssetSidecarRecord`] / [`serialize_sidecar`] / [`deserialize_sidecar`] —
//!   the canonical CBOR sidecar format, with unknown fields preserved verbatim.
//! - [`CatalogError`] — the single error type crossing the boundary.

uniffi::setup_scaffolding!();

mod catalog;
mod error;
mod records;
mod sidecar;

pub use catalog::Catalog;
pub use error::CatalogError;
pub use records::{AlbumRecord, AssetRecord, AssetStackRecord, StackMemberRecord};
pub use sidecar::{AssetSidecarRecord, StackHintRecord, deserialize_sidecar, serialize_sidecar};

/// Initialise structured logging for the Rust core.
///
/// On Apple platforms this routes `capsule-core`'s `log` records into the
/// unified logging system, where they are queryable via Console.app or the
/// `log` CLI. Safe to call more than once — subsequent calls are ignored.
#[uniffi::export]
pub fn init_logging() {
    #[cfg(target_vendor = "apple")]
    {
        let _ = oslog::OsLogger::new("com.justin13888.capsule.core")
            .level_filter(log::LevelFilter::Trace)
            .init();
    }
}
