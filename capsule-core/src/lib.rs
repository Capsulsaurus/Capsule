pub mod backup;
pub mod cbor;
pub mod cohort;
pub mod constants;
pub mod crypto;
pub mod db;
pub mod domain;
pub mod drop;
pub mod exif;
pub mod import;
pub mod library;
pub mod lifecycle;
#[cfg(feature = "media")]
pub mod media;
pub mod metadata;
pub mod models;
pub mod sharing;
pub mod sidecar;
pub mod utils;
pub mod validation;

/// uniffi-generated bindings surface for Kotlin/Swift (`ffi` feature). The exported API is a
/// thin wrapper over [`lifecycle::Workspace`]; see [`ffi`].
#[cfg(feature = "ffi")]
pub mod ffi;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();
