//! The server's operations, one module per surface.
//!
//! Modules are added here as each lane-C slice is ported from `legacy-review/server-salvo/`.
//! `legacy-review/server-salvo/REVIEW.md` is explicit about what may and may not come across:
//! the authentication primitives, upload offset/checksum/terminal-state handling, filesystem
//! staging and the Postgres entities are migration *input*; the Salvo handlers, response
//! writers, OpenAPI registration and configuration projections are not.

pub mod auth;
pub mod blob;
pub mod sync;
pub mod upload;
pub mod version;
