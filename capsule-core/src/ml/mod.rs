//! On-device ML orchestration and the model inventory seam (SSoT: [AI/ML Integrations]).
//!
//! This module owns the *structure* the AI design fixes — independent of which model actually
//! runs:
//!
//! - the [`registry`] — a known-models seam (one row per task) and the
//!   **embedding-provenance invariant** (every embedding carries `(model_id, model_version)`;
//!   the [vector index](crate::db::vector) refuses inserts from **unknown** models, while a
//!   *superseded-but-known* version is admitted as a stale-flagged row and excluded from
//!   queries until regenerated). The canonical inventory rows and the version-bump regeneration
//!   orchestration are slice `S-H2`; this seam carries the minimum the index needs.
//!
//! Real per-platform inference is deferred behind a `ModelRunner` seam (a later slice), exactly
//! as live MLS group state is deferred behind
//! [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority) with `ReferenceAuthority` standing
//! in. The local vector index lives in [`crate::db::vector`]. No model weights are committed to
//! this repository.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

pub mod registry;

pub use registry::{
    DistanceMetric, EmbeddingDim, ModelId, ModelRow, ModelVersion, Registry, RegistryError,
    TaskKind, TaskOutput,
};
