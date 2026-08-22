//! On-device ML orchestration and the model inventory seam (SSoT: [AI/ML Integrations]).
//!
//! This module owns the *structure* the AI design fixes — independent of which model actually
//! runs:
//!
//! - the [`registry`] — the **canonical model inventory** (one v1-committed row per task) and the
//!   **embedding-provenance invariant** (every embedding carries `(model_id, model_version)`;
//!   the [vector index](crate::db::vector) refuses inserts from **unknown** models, while a
//!   *superseded-but-known* version is admitted as a stale-flagged row and excluded from
//!   queries until regenerated). It owns the version-bump [swap
//!   primitive](registry::Registry::bump_version);
//! - the [`regen`] loop — the background per-asset **regeneration orchestration** that consumes
//!   the staleness a swap creates: it walks
//!   [`stale_embedding_assets`](crate::db::DatabaseDriver::stale_embedding_assets) and re-embeds
//!   each asset at the new canonical version through an injected [`Embedder`](regen::Embedder)
//!   seam, resumably and per-asset (never a global truncate).
//!
//! - the [`runner`] seam — the [`ModelRunner`](runner::ModelRunner) trait every consumer routes
//!   per-task inference over decoded pixels through, and the deterministic
//!   [`FixtureRunner`](runner::FixtureRunner) double. Real per-platform runners (ONNX/CoreML/NNAPI,
//!   the CLIP runner) ride the **default-off, weight-fetching `inference` feature** — a follow-up,
//!   never part of the default gate;
//! - the [`orchestrator`] — the deterministic execution path for the v1-committed slots: run the
//!   canonical model over an asset, land semantic + face vectors in the right `vec0` partition
//!   ([platform-partition fallback](orchestrator::resolve_partition)), and write zero-shot
//!   [AI tags](orchestrator::auto_tag) into `tags_ai` as a signed metadata update through the
//!   [`AiTagSink`](orchestrator::AiTagSink) seam — implemented by
//!   [`Workspace`](crate::lifecycle::Workspace), named as a trait so `ml` does not import
//!   `lifecycle` (which imports `ml`); plus the device-bound batching/thermal policy.
//!
//! Real per-platform inference is deferred behind the [`Embedder`](regen::Embedder) /
//! [`ModelRunner`](runner::ModelRunner) seams (the real runner is a later slice), exactly as live
//! MLS group state is deferred behind [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority)
//! with `ReferenceAuthority` standing in; the deterministic
//! [`DeterministicEmbedder`](regen::DeterministicEmbedder) and
//! [`FixtureRunner`](runner::FixtureRunner) drive every path with no model weights. The local
//! vector index lives in [`crate::db::vector`]. No model weights are committed to this repository.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

pub mod orchestrator;
pub mod regen;
pub mod registry;
pub mod runner;

pub use orchestrator::{
    AiTagSink, AssetSource, BatchMode, CANONICAL_PARTITION, KnownAnswer, OrchestratorError,
    StoreError, auto_tag, choose_batch_mode, embed_and_store, micro_batch_size, resolve_partition,
    semantic_search, should_pause_for_heat,
};
pub use regen::{DeterministicEmbedder, Embedder, RegenError, RegenReport, regenerate_stale};
pub use registry::{
    DistanceMetric, EmbeddingDim, ModelId, ModelRow, ModelVersion, Registry, RegistryError,
    TaskKind, TaskOutput,
};
pub use runner::{BBox, Detection, Embedding, FixtureRunner, Frame, ModelRunner, RunnerError};
