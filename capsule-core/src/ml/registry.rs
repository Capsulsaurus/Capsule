//! The known-models seam and the embedding-provenance invariant (SSoT: [AI/ML —
//! Models and Algorithms] + [AI/ML — Embedding Provenance]).
//!
//! One [`ModelRow`] per task. Every embedding Capsule stores carries the tuple
//! `(model_id, model_version)` identifying which row produced it, and:
//!
//! - the vector index **refuses inserts** whose `model_id` is **unknown to the inventory**
//!   ([`Registry::check_insert`]) — a buggy or new client producing embeddings from an
//!   unrecognized model is rejected at the insert API, never silently mixed in;
//! - a *superseded-but-known* version (a canonical model at an older version) is **admitted as a
//!   stale-flagged row** ([`Registry::is_stale`]) — it is the regeneration queue and is excluded
//!   from queries until regenerated from the originals; the refusal targets unknown models, not
//!   known-but-old ones;
//! - a model swap **increments `model_version`**; pre-swap entries become stale by the same rule;
//! - cross-`(model_id, model_version)` comparison is forbidden — vector spaces differ.
//!
//! Every `model_id` is declared in **exactly one** row, so swapping a model is a one-row edit
//! that propagates by `model_id` to every consumer. This module owns the *canonical inventory
//! rows* (the v1-committed slots, enriched with the function and fallback the contract names) and
//! the version-bump **swap primitive** ([`Registry::bump_version`]); the background per-asset
//! **regeneration orchestration** that consumes the resulting staleness lives in
//! [`crate::ml::regen`], and the provenance gate the [vector index](crate::ml::EmbeddingInsert) calls is
//! [`Registry::check_insert`].
//!
//! [AI/ML — Models and Algorithms]: https://docs/design/ai/#models-and-algorithms
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

// The identity vocabulary these rows are written in lives in the `domain` leaf — `db` names its
// `vec0` columns with the same types, and importing them from `ml` closed a `db -> ml -> db`
// cycle. Re-exported so `crate::ml::TaskKind` (and every other spelling) still resolves.
pub use crate::domain::model_identity::{
    DistanceMetric, EmbeddingDim, ModelId, ModelVersion, RegistryError, TaskKind,
};
use crate::ml::index::{EmbeddingProvenance, VectorTableSpec};

/// What a task produces: a stored embedding vector, or detections that feed downstream stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutput {
    /// A normalized embedding stored in the vector index.
    Embedding {
        /// Vector dimensionality.
        dim: EmbeddingDim,
        /// Ranking metric.
        metric: DistanceMetric,
    },
    /// Bounding boxes / landmarks (not stored as vectors here).
    Detection,
}

/// One inventory row: the canonical model for a [`TaskKind`] — a v1-committed slot from
/// [AI/ML — v1-Committed Slots], carrying the model, its function, and its named fallback.
///
/// [AI/ML — v1-Committed Slots]: https://docs/design/ai/#v1-committed-slots
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// The task this row serves.
    pub task: TaskKind,
    /// The canonical model id (unique across the whole inventory).
    pub model_id: ModelId,
    /// The current canonical version. A swap bumps this and flags prior entries stale.
    pub canonical_version: ModelVersion,
    /// What the model produces.
    pub output: TaskOutput,
    /// Human-readable label (logging / demo only).
    pub display_name: &'static str,
    /// The slot's function — the contract's Function column for this row.
    pub function: &'static str,
    /// The named fallback model the contract keeps for this slot if the canonical choice proves
    /// insufficient in field testing (`None` where the contract names none). Not a second live
    /// model: swapping to it is a [version bump](Registry::bump_version) like any other.
    pub fallback: Option<&'static str>,
}

impl ModelRow {
    /// The embedding `(dim, metric)` if this row produces stored embeddings.
    pub fn embedding_spec(&self) -> Option<(EmbeddingDim, DistanceMetric)> {
        match self.output {
            TaskOutput::Embedding { dim, metric } => Some((dim, metric)),
            TaskOutput::Detection => None,
        }
    }

    /// The derivative `format` string for this model's embeddings, e.g. `embedding/mobileclip-b`
    /// — the value carried in [`DerivativeCore::format`](crate::crypto::provenance::DerivativeCore::format).
    pub fn embedding_format(&self) -> String {
        format!("embedding/{}", self.model_id)
    }
}

/// The model inventory seam and the embedding-provenance gate.
#[derive(Debug, Clone)]
pub struct Registry {
    rows: Vec<ModelRow>,
}

impl Registry {
    /// The v1-committed inventory — the four launch slots of [AI/ML — v1-Committed Slots]:
    /// MobileCLIP-B, YOLOv10, SCRFD, AdaFace, each with the contract's function and named
    /// fallback. This is the seam the vector index gates against and the regeneration loop walks.
    ///
    /// [AI/ML — v1-Committed Slots]: https://docs/design/ai/#v1-committed-slots
    pub fn canonical() -> Self {
        let rows = vec![
            ModelRow {
                task: TaskKind::SemanticSearch,
                model_id: ModelId("mobileclip-b".into()),
                canonical_version: ModelVersion("1".into()),
                output: TaskOutput::Embedding {
                    dim: EmbeddingDim(512),
                    metric: DistanceMetric::Cosine,
                },
                display_name: "MobileCLIP-B",
                function: "Global image embedding for natural-language + similarity search; \
                           sized for the lowest-end device.",
                fallback: Some("quantized SigLIP-tiny"),
            },
            ModelRow {
                task: TaskKind::ObjectDetection,
                model_id: ModelId("yolov10".into()),
                canonical_version: ModelVersion("1".into()),
                output: TaskOutput::Detection,
                display_name: "YOLOv10",
                function: "Object/background detection feeding dense tagging; the backbone is \
                           reused for person detection.",
                fallback: None,
            },
            ModelRow {
                task: TaskKind::FaceDetection,
                model_id: ModelId("scrfd".into()),
                canonical_version: ModelVersion("1".into()),
                output: TaskOutput::Detection,
                display_name: "SCRFD",
                function: "Efficient face bounding-box + landmark detection.",
                fallback: None,
            },
            ModelRow {
                task: TaskKind::FaceRecognition,
                model_id: ModelId("adaface".into()),
                canonical_version: ModelVersion("1".into()),
                output: TaskOutput::Embedding {
                    dim: EmbeddingDim(512),
                    metric: DistanceMetric::Cosine,
                },
                display_name: "InsightFace (AdaFace)",
                function: "Face embeddings; AdaFace handles low-quality/dark images well.",
                fallback: None,
            },
        ];
        // SSoT invariant: every model_id appears in exactly one row, and every task has a row.
        debug_assert!(Self::ids_unique(&rows), "duplicate model_id in inventory");
        debug_assert!(
            TaskKind::ALL
                .iter()
                .all(|t| rows.iter().any(|r| r.task == *t)),
            "every task must have a canonical row"
        );
        Self { rows }
    }

    fn ids_unique(rows: &[ModelRow]) -> bool {
        let mut ids: Vec<&str> = rows.iter().map(|r| r.model_id.as_str()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        ids.len() == n
    }

    /// Every inventory row.
    pub fn rows(&self) -> &[ModelRow] {
        &self.rows
    }

    /// The canonical row for `task`.
    pub fn canonical_for(&self, task: TaskKind) -> Option<&ModelRow> {
        self.rows.iter().find(|r| r.task == task)
    }

    /// Record a model swap for `task` — a one-row edit setting its canonical version. Embeddings
    /// stored at the prior version become [stale](Self::is_stale) and are excluded from queries
    /// until [regenerated](crate::ml::regenerate_stale) from the originals.
    pub fn set_canonical_version(&mut self, task: TaskKind, version: ModelVersion) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.task == task) {
            row.canonical_version = version;
        }
    }

    /// Swap the model for `task` by **incrementing** its canonical `model_version` — the contract's
    /// "a model swap increments `model_version` for that task" ([AI/ML — Embedding Provenance]).
    /// A numeric version is treated as an integer generation and advanced by one; a non-numeric
    /// version gets a `+1` suffix so the tuple still changes. Returns the new canonical version, or
    /// `None` if `task` has no row. This is the swap half of the version-bump regeneration loop —
    /// its regeneration half is [`regen::regenerate_stale`](crate::ml::regenerate_stale),
    /// which walks the now-stale entries [`stale_embedding_assets`](crate::ml::VectorIndex::stale_embedding_assets)
    /// reports and replaces each per-asset.
    ///
    /// [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance
    pub fn bump_version(&mut self, task: TaskKind) -> Option<ModelVersion> {
        let row = self.rows.iter_mut().find(|r| r.task == task)?;
        let next = match row.canonical_version.as_str().parse::<u64>() {
            Ok(n) => (n.wrapping_add(1)).to_string(),
            Err(_) => format!("{}+1", row.canonical_version),
        };
        row.canonical_version = ModelVersion::from(next);
        Some(row.canonical_version.clone())
    }

    /// The row a `model_id` belongs to, if any (each id is in at most one row).
    pub fn row_for_id(&self, model_id: &ModelId) -> Option<&ModelRow> {
        self.rows.iter().find(|r| &r.model_id == model_id)
    }

    /// Whether `model_id` is a known model of some task.
    pub fn is_canonical(&self, model_id: &ModelId) -> bool {
        self.row_for_id(model_id).is_some()
    }

    /// The embedding dimension for `task`, if it produces embeddings.
    pub fn dim_for(&self, task: TaskKind) -> Option<EmbeddingDim> {
        self.canonical_for(task)?.embedding_spec().map(|(d, _)| d)
    }

    /// The ranking metric for `task`, if it produces embeddings.
    pub fn metric_for(&self, task: TaskKind) -> Option<DistanceMetric> {
        self.canonical_for(task)?.embedding_spec().map(|(_, m)| m)
    }

    /// Whether `(model_id, version)` is the *current* canonical pair for `task`.
    pub fn is_current(&self, task: TaskKind, model_id: &ModelId, version: &ModelVersion) -> bool {
        self.canonical_for(task)
            .is_some_and(|r| &r.model_id == model_id && &r.canonical_version == version)
    }

    /// Whether an entry tagged `(model_id, version)` for `task` is **stale** — i.e. the task has a
    /// canonical row but the pair is not the current one. Stale entries are admitted but excluded
    /// from queries until regenerated from the originals.
    pub fn is_stale(&self, task: TaskKind, model_id: &ModelId, version: &ModelVersion) -> bool {
        self.canonical_for(task).is_some() && !self.is_current(task, model_id, version)
    }

    /// The embedding-provenance gate the vector index calls on every insert: accept any version of
    /// the **known** model for an embedding task, returning its `(dim, metric)`. An **unknown**
    /// model or a non-embedding task is refused; a *superseded-but-known* version is **not**
    /// refused here — it is admitted (and, being non-current, [stale](Self::is_stale)).
    pub fn check_insert(
        &self,
        task: TaskKind,
        model_id: &ModelId,
        _version: &ModelVersion,
    ) -> Result<(EmbeddingDim, DistanceMetric), RegistryError> {
        let row = self
            .canonical_for(task)
            .ok_or_else(|| RegistryError::NonCanonical {
                task,
                model_id: model_id.clone(),
            })?;
        if &row.model_id != model_id {
            return Err(RegistryError::NonCanonical {
                task,
                model_id: model_id.clone(),
            });
        }
        row.embedding_spec()
            .ok_or(RegistryError::NotAnEmbeddingTask { task })
    }
}

/// The partition tables this inventory's embedding slots need — the `db`-side description of the
/// `vec0` schema, with no `ml` type in sight. Hand it to
/// `DatabaseDriver::create_vector_tables` to
/// materialize the whole schema up front instead of on first use.
impl Registry {
    /// One [`VectorTableSpec`] per embedding-producing slot (detection slots store no vectors).
    pub fn vector_tables(&self) -> Vec<VectorTableSpec> {
        self.rows
            .iter()
            .filter_map(|r| {
                r.embedding_spec().map(|(dim, metric)| VectorTableSpec {
                    task: r.task,
                    dim,
                    metric,
                })
            })
            .collect()
    }
}

/// The inventory seen through the vector index's eyes: the three questions `db` asks, answered by
/// delegating to the inherent inventory methods. This impl **is** the inverted `db -> ml` edge —
/// `db` names the trait, `ml` supplies the implementation, so the dependency now runs one way.
impl EmbeddingProvenance for Registry {
    fn check_insert(
        &self,
        task: TaskKind,
        model_id: &ModelId,
        version: &ModelVersion,
    ) -> Result<(EmbeddingDim, DistanceMetric), RegistryError> {
        Registry::check_insert(self, task, model_id, version)
    }

    fn embedding_spec(&self, task: TaskKind) -> Option<(EmbeddingDim, DistanceMetric)> {
        self.canonical_for(task)?.embedding_spec()
    }

    fn canonical_version(&self, task: TaskKind) -> Option<ModelVersion> {
        self.canonical_for(task)
            .map(|r| r.canonical_version.clone())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::canonical()
    }
}

#[cfg(test)]
mod tests;
