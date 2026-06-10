//! On-device ML orchestration and the canonical model inventory (SSoT: [AI/ML Integrations]).
//!
//! This module owns the *structure* the AI design fixes — independent of which model actually
//! runs:
//!
//! - the [`registry`] — the canonical model inventory (one row per task) and the
//!   **embedding-provenance invariant** (every embedding carries `(model_id, model_version)`;
//!   the vector index refuses non-canonical inserts; a version bump flags old entries stale);
//!
//! Real per-platform inference is deferred behind a `ModelRunner` seam (a later PR), exactly as
//! live MLS group state is deferred behind
//! [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority) with `ReferenceAuthority` standing
//! in. The portable runner is a default-off, weight-fetching feature of this crate; the local
//! vector index lives in [`crate::db`]. No model weights are committed to this repository.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

pub mod registry;

pub use registry::{
    DistanceMetric, EmbeddingDim, ModelId, ModelRow, ModelVersion, Registry, RegistryError,
    TaskKind, TaskOutput,
};
