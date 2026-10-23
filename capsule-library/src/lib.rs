//! All of a Capsule client's local state, over the stateless `capsule-core`.
//!
//! This crate owns what `capsule-core` may not: the SQLite index ([`db`]), the on-disk library
//! and its lock ([`library`]), the [`Workspace`](lifecycle::Workspace) and its persistence
//! ([`lifecycle`]), import scanning and execution ([`import`]), sidecar and album-store file I/O,
//! backup file I/O, file-path EXIF reading ([`exif`]) and the desktop TPM signers ([`signers`]).
//! Every cryptographic, codec and planning decision stays in `capsule-core`; the code here reads
//! and writes the bytes those decisions are made over. See
//! [Module Map — Stateless Core](https://docs/design/module-map/#stateless-core).

pub mod album_store;
pub mod db;
pub mod exif;
pub mod file_metadata;
pub mod import;
pub mod library;
pub mod lifecycle;
pub mod sidecar_io;
pub mod signers;
pub mod utils;

#[cfg(test)]
mod test_heap;
