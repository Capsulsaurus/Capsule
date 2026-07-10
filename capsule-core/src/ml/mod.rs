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
//! Real per-platform inference is deferred behind the [`Embedder`](regen::Embedder) seam (the
//! on-device runner is a later slice), exactly as live MLS group state is deferred behind
//! [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority) with `ReferenceAuthority` standing
//! in; the deterministic [`DeterministicEmbedder`](regen::DeterministicEmbedder) drives the loop
//! with no model weights. The local vector index lives in [`crate::db::vector`]. No model weights
//! are committed to this repository.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

pub mod regen;
pub mod registry;

pub use regen::{DeterministicEmbedder, Embedder, RegenError, RegenReport, regenerate_stale};
pub use registry::{
    DistanceMetric, EmbeddingDim, ModelId, ModelRow, ModelVersion, Registry, RegistryError,
    TaskKind, TaskOutput,
};
