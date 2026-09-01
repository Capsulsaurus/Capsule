//! Media decode/encode and metadata utilities (formerly the standalone
//! `capsule-media` crate), behind the non-default `media` feature. Owner doc:
//! Thumbnails and Previews; thumbnail/LQIP generation over these utilities is
//! slice `S-B1` in the repo-root `SLICES.md`.

pub mod core;
pub mod fs;
pub mod image;
pub mod metadata;
pub mod video;
