//! On-device ML orchestration and the canonical model inventory (SSoT: [AI/ML Integrations]).
//!
//! - [`registry`] — the canonical model inventory (one row per task) and the
//!   **embedding-provenance invariant** (every embedding carries `(model_id, model_version)`;
//!   the vector index refuses non-canonical inserts; a version bump flags old entries stale).
//! - [`runner`] — the [`ModelRunner`] inference seam and a deterministic [`FixtureRunner`] for
//!   tests. Real per-platform runners are a default-off, weight-fetching feature; this mirrors
//!   [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority) /
//!   [`ReferenceAuthority`](crate::crypto::authority::ReferenceAuthority) standing in for live MLS.
//! - [`orchestrator`] — runs the canonical model over an asset and writes results back through the
//!   signed lifecycle (embeddings, zero-shot AI tags), plus the device-bound batching/thermal
//!   policy and natural-language search.
//! - [`video`] — video-as-sparse-photos keyframe selection.
//! - [`reid`] — re-identification & pseudo-labeling geometry/vector math.
//!
//! The local vector index lives in [`crate::db`]. No model weights are committed to this repo.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

pub mod orchestrator;
pub mod registry;
pub mod reid;
pub mod runner;
pub mod video;

pub use orchestrator::{
    BatchMode, OrchestratorError, auto_tag, choose_batch_mode, embed_and_store, micro_batch_size,
    semantic_search, should_pause_for_heat,
};
pub use registry::{
    DistanceMetric, EmbeddingDim, ModelId, ModelRow, ModelVersion, Registry, RegistryError,
    TaskKind, TaskOutput,
};
pub use reid::{cosine_sim, iou, links_to_profile, pseudo_labels};
pub use runner::{BBox, Detection, Embedding, FixtureRunner, Frame, ModelRunner, RunnerError};
pub use video::{Keyframe, Scene, reject_blurry, sample_keyframes};
