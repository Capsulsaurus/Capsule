//! The vector-index seam — the [`VectorIndex`] port the orchestration and the regeneration loop
//! write embeddings through, and the value types that cross it (SSoT: [AI/ML — Database
//! Indexing] + [AI/ML — Embedding Provenance]).
//!
//! `capsule-core` is stateless, so the index itself — SQLite plus `sqlite-vec`'s `vec0` tables —
//! lives in `capsule-library`, which implements this trait for its catalog. Core names only the
//! questions the ML layer asks of an index: insert a provenanced embedding, rank the nearest
//! neighbours in the current canonical partition, and list the assets whose stored embedding is
//! stale. Everything the index needs to answer them about the model inventory reaches it through
//! the [`EmbeddingProvenance`] gate, implemented by the [`Registry`](crate::ml::Registry).
//!
//! [AI/ML — Database Indexing]: https://docs/design/ai/#database-indexing-and-view-generation
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use thiserror::Error;

use crate::domain::model_identity::{
    DistanceMetric, EmbeddingDim, ModelId, ModelVersion, RegistryError, TaskKind,
};

/// A provenanced embedding to insert into the index: the subject asset, the producing model
/// `(task, model_id, model_version)`, the `platform` partition discriminator, and the vector.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingInsert<'a> {
    /// The asset the embedding was produced from.
    pub asset_id: &'a str,
    /// The task that produced it.
    pub task: TaskKind,
    /// The producing model id.
    pub model_id: &'a ModelId,
    /// The producing model version.
    pub model_version: &'a ModelVersion,
    /// The platform partition (incomparable across platforms; see the E2EE fallback in `ai.md`).
    pub platform: &'a str,
    /// The embedding vector (length must equal the task's registry-declared dimension).
    pub vector: &'a [f32],
}

/// A single nearest-neighbour hit.
#[derive(Debug, Clone, PartialEq)]
pub struct KnnHit {
    /// The matched asset id.
    pub asset_id: String,
    /// Distance under the task's metric (smaller = nearer; cosine over normalized vectors).
    pub distance: f64,
}

/// A stored embedding's provenance (from the companion table; the vector itself stays in `vec0`).
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingRecord {
    /// Asset the embedding was produced from.
    pub asset_id: String,
    /// Task that produced it.
    pub task: TaskKind,
    /// Platform partition discriminator.
    pub platform: String,
    /// Producing model.
    pub model_id: ModelId,
    /// Producing model version.
    pub model_version: ModelVersion,
}

/// Failures from the vector index.
#[derive(Debug, Error)]
pub enum VectorIndexError {
    /// The embedding-provenance invariant rejected the insert (an unknown model or a
    /// non-embedding task).
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The vector's length does not match the task's registry-declared dimension.
    #[error("embedding dimension mismatch for {task:?}: expected {expected}, got {got}")]
    DimMismatch {
        /// The task.
        task: TaskKind,
        /// Registry-declared dimension.
        expected: usize,
        /// The supplied vector length.
        got: usize,
    },
    /// The index's backing store failed (a SQLite error, for the library's catalog). Type-erased
    /// so `capsule-core` never names a storage engine.
    #[error("vector store: {0}")]
    Store(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The shape of one task's `vec0` partition table: which task it serves, and the `(dim, metric)`
/// its vectors are sized and ranked by.
///
/// A vector table cannot be created before something declares that shape, and the shape is a
/// property of the *model* that writes into it — so the index takes it from the writer rather than
/// guessing at open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorTableSpec {
    /// The task whose partition table this describes.
    pub task: TaskKind,
    /// Vector dimensionality (the `vec0` `float[N]` column width).
    pub dim: EmbeddingDim,
    /// The metric the partition ranks by.
    pub metric: DistanceMetric,
}

/// The embedding-provenance gate the vector index calls before it stores or ranks anything —
/// the seam through which the model inventory reaches the index **without the index depending on
/// `ml`**.
///
/// Implemented by the ML layer's `Registry`, which owns the canonical rows and the version-bump
/// swap primitive. The index only ever asks the three questions below, so the rest of the
/// inventory (display names, fallbacks, swap history) stays out of that layer entirely.
pub trait EmbeddingProvenance {
    /// The gate on every insert: accept any version of a **known** model for an embedding task,
    /// returning the `(dim, metric)` its vectors must match. An **unknown** model or a
    /// non-embedding task is refused; a *superseded-but-known* version is **not** refused — it is
    /// admitted and lands as a stale-flagged row.
    fn check_insert(
        &self,
        task: TaskKind,
        model_id: &ModelId,
        version: &ModelVersion,
    ) -> Result<(EmbeddingDim, DistanceMetric), RegistryError>;

    /// The `(dim, metric)` of `task`'s vector space, or `None` if `task` stores no embeddings.
    fn embedding_spec(&self, task: TaskKind) -> Option<(EmbeddingDim, DistanceMetric)>;

    /// The **current** canonical `model_version` for `task` — the partition queries are answered
    /// from — or `None` if the task has no canonical model.
    fn canonical_version(&self, task: TaskKind) -> Option<ModelVersion>;

    /// The partition table shape for `task`, if it stores embeddings.
    fn vector_table(&self, task: TaskKind) -> Option<VectorTableSpec> {
        let (dim, metric) = self.embedding_spec(task)?;
        Some(VectorTableSpec { task, dim, metric })
    }
}

/// The store port the ML layer writes embeddings into and ranks them from.
///
/// Partitioned by `(platform, model_version)`: a query answers from the **current** canonical
/// version only, so a stale embedding (an older version, left over after a model swap) is excluded
/// structurally until [`regenerate_stale`](crate::ml::regenerate_stale) replaces it. The production
/// implementor is the library catalog; an implementation must honour these laws:
///
/// - [`insert_embedding`](Self::insert_embedding) refuses what
///   [`EmbeddingProvenance::check_insert`] refuses and a vector whose length is not the declared
///   dimension, and **replaces** any prior embedding for the same `(asset, task, platform)`;
/// - [`knn`](Self::knn) returns hits nearest-first from the `platform` partition at the
///   registry's current canonical version only;
/// - [`stale_embedding_assets`](Self::stale_embedding_assets) lists, sorted, the assets in the
///   `platform` partition whose stored version trails the canonical one.
pub trait VectorIndex {
    /// Insert (or replace) one provenanced embedding.
    fn insert_embedding<P: EmbeddingProvenance>(
        &self,
        registry: &P,
        e: EmbeddingInsert<'_>,
    ) -> Result<(), VectorIndexError>;

    /// The `k` nearest neighbours of `query` in `task`'s current canonical `platform` partition.
    fn knn<P: EmbeddingProvenance>(
        &self,
        registry: &P,
        task: TaskKind,
        query: &[f32],
        k: usize,
        platform: &str,
    ) -> Result<Vec<KnnHit>, VectorIndexError>;

    /// The regeneration work-list: assets in `task`'s `platform` partition whose embedding is
    /// stale under `registry`, sorted by asset id.
    fn stale_embedding_assets<P: EmbeddingProvenance>(
        &self,
        registry: &P,
        task: TaskKind,
        platform: &str,
    ) -> Result<Vec<String>, VectorIndexError>;
}
