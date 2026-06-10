//! The canonical model inventory and the embedding-provenance invariant (SSoT: [AI/ML —
//! Models and Algorithms] + [AI/ML — Embedding Provenance]).
//!
//! One [`ModelRow`] per task. Every embedding Capsule stores carries the tuple
//! `(model_id, model_version)` identifying which row produced it, and:
//!
//! - the vector index **refuses inserts** whose `(model_id, model_version)` is not the current
//!   canonical row for its task ([`Registry::check_insert`]);
//! - a model swap **increments `model_version`**; pre-swap entries are **flagged stale**
//!   ([`Registry::is_stale`]) and excluded from queries until regenerated from the originals;
//! - cross-`(model_id, model_version)` comparison is forbidden — vector spaces differ.
//!
//! Every `model_id` is declared in **exactly one** row, so swapping a model is a one-row edit
//! that propagates by `model_id` to every consumer.
//!
//! [AI/ML — Models and Algorithms]: https://docs/design/ai/#models-and-algorithms
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A canonical ML task — the v1-committed launch pipeline ([AI/ML — v1-Committed Slots]).
///
/// Closed enum per protocol version: an unknown value is a **structural error**, never a
/// "future value to ignore". Post-v1 candidate tasks each commit to a full inventory row (and a
/// new variant here) when they ship.
///
/// [AI/ML — v1-Committed Slots]: https://docs/design/ai/#v1-committed-slots
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// Global image embedding for natural-language + similarity search (MobileCLIP-B).
    SemanticSearch,
    /// Object/background detection feeding dense tagging (YOLOv10).
    ObjectDetection,
    /// Face bounding-box + landmark detection (SCRFD).
    FaceDetection,
    /// Face embedding for matching/clustering (InsightFace AdaFace).
    FaceRecognition,
}

impl TaskKind {
    /// Every committed task, in inventory order.
    pub const ALL: [TaskKind; 4] = [
        TaskKind::SemanticSearch,
        TaskKind::ObjectDetection,
        TaskKind::FaceDetection,
        TaskKind::FaceRecognition,
    ];
}

/// A model identifier (stable across versions; e.g. `mobileclip-b`). Declared in exactly one
/// [`ModelRow`]. Serializes transparently as its string so it interoperates with the
/// `model_id` fields on [`AiTag`](crate::sidecar::sidecar_v1::AiTag) and
/// [`DerivativeCore`](crate::crypto::provenance::manifest::DerivativeCore).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(pub String);

/// A model version. Bumped on every model swap for a task; old embeddings at a prior version are
/// flagged stale. Serializes transparently as its string (see [`ModelId`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelVersion(pub String);

macro_rules! str_newtype {
    ($t:ty) => {
        impl $t {
            /// The underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl From<&str> for $t {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
        impl From<String> for $t {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
str_newtype!(ModelId);
str_newtype!(ModelVersion);

/// The dimensionality of an embedding vector for an embedding-producing task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EmbeddingDim(pub u32);

impl EmbeddingDim {
    /// The dimension as a `usize` (for buffer sizing).
    pub fn get(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for EmbeddingDim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The distance metric a vector index ranks an embedding task by.
///
/// Embeddings are L2-normalized, so **cosine distance ranks identically to the inner product**
/// — this is the design's `<#>` inner-product operator intent, expressed with the metric the
/// SQLite `vec0` engine implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DistanceMetric {
    /// Cosine distance over normalized vectors (== inner-product ranking).
    Cosine,
    /// Squared Euclidean (L2) distance.
    L2,
}

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

/// One inventory row: the canonical model for a [`TaskKind`].
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
    /// — the value carried in [`DerivativeCore::format`](crate::crypto::provenance::manifest::DerivativeCore::format).
    pub fn embedding_format(&self) -> String {
        format!("embedding/{}", self.model_id)
    }
}

/// Refusals from the embedding-provenance invariant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// The `model_id` is not the canonical model for the task.
    #[error("model `{model_id}` is not canonical for task {task:?}")]
    NonCanonical {
        /// The task.
        task: TaskKind,
        /// The offending model id.
        model_id: ModelId,
    },
    /// The `model_id` is canonical but the `model_version` is not the current one (stale).
    #[error(
        "model `{model_id}` version `{version}` is stale for task {task:?} (canonical `{canonical}`)"
    )]
    Stale {
        /// The task.
        task: TaskKind,
        /// The model id.
        model_id: ModelId,
        /// The presented (stale) version.
        version: ModelVersion,
        /// The current canonical version.
        canonical: ModelVersion,
    },
    /// The task does not produce stored embeddings (e.g. a detection task).
    #[error("task {task:?} does not produce stored embeddings")]
    NotAnEmbeddingTask {
        /// The task.
        task: TaskKind,
    },
}

/// The canonical model inventory and the embedding-provenance gate.
#[derive(Debug, Clone)]
pub struct Registry {
    rows: Vec<ModelRow>,
}

impl Registry {
    /// The v1-committed inventory: MobileCLIP-B, YOLOv10, SCRFD, AdaFace.
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
            },
            ModelRow {
                task: TaskKind::ObjectDetection,
                model_id: ModelId("yolov10".into()),
                canonical_version: ModelVersion("1".into()),
                output: TaskOutput::Detection,
                display_name: "YOLOv10",
            },
            ModelRow {
                task: TaskKind::FaceDetection,
                model_id: ModelId("scrfd".into()),
                canonical_version: ModelVersion("1".into()),
                output: TaskOutput::Detection,
                display_name: "SCRFD",
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
    /// until regenerated from the originals.
    pub fn set_canonical_version(&mut self, task: TaskKind, version: ModelVersion) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.task == task) {
            row.canonical_version = version;
        }
    }

    /// Swap the canonical *model* for `task` — a one-row edit of both `model_id` and version (the
    /// embedding dimension and metric are unchanged). Used when a deployment runs a different model
    /// than the inventory default (e.g. a base model pending the committed choice); the registry is
    /// the SSoT, so the runner declaring this model is then accepted by the index.
    pub fn set_canonical_model(
        &mut self,
        task: TaskKind,
        model_id: ModelId,
        version: ModelVersion,
    ) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.task == task) {
            row.model_id = model_id;
            row.canonical_version = version;
        }
    }

    /// The row a `model_id` belongs to, if any (each id is in at most one row).
    pub fn row_for_id(&self, model_id: &ModelId) -> Option<&ModelRow> {
        self.rows.iter().find(|r| &r.model_id == model_id)
    }

    /// Whether `model_id` is the canonical model of some task.
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
    /// canonical row but the pair is not the current one. Stale entries are excluded from queries
    /// until regenerated from the originals.
    pub fn is_stale(&self, task: TaskKind, model_id: &ModelId, version: &ModelVersion) -> bool {
        self.canonical_for(task).is_some() && !self.is_current(task, model_id, version)
    }

    /// The embedding-provenance gate the vector index calls on every insert: accept only the
    /// current canonical `(model_id, version)` for an embedding task, returning its `(dim, metric)`.
    /// A non-canonical model, a stale version, or a non-embedding task is refused.
    pub fn check_insert(
        &self,
        task: TaskKind,
        model_id: &ModelId,
        version: &ModelVersion,
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
        if &row.canonical_version != version {
            return Err(RegistryError::Stale {
                task,
                model_id: model_id.clone(),
                version: version.clone(),
                canonical: row.canonical_version.clone(),
            });
        }
        row.embedding_spec()
            .ok_or(RegistryError::NotAnEmbeddingTask { task })
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::canonical()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor;

    #[test]
    fn every_task_has_exactly_one_canonical_row() {
        let reg = Registry::canonical();
        for task in TaskKind::ALL {
            let matches: Vec<_> = reg.rows().iter().filter(|r| r.task == task).collect();
            assert_eq!(matches.len(), 1, "task {task:?} must have exactly one row");
        }
        assert_eq!(reg.rows().len(), TaskKind::ALL.len());
    }

    #[test]
    fn every_model_id_is_declared_in_exactly_one_row() {
        let reg = Registry::canonical();
        let mut ids: Vec<&str> = reg.rows().iter().map(|r| r.model_id.as_str()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "every model_id must be unique");
    }

    #[test]
    fn lookup_by_id_resolves_the_owning_row() {
        let reg = Registry::canonical();
        let row = reg.row_for_id(&ModelId::from("mobileclip-b")).unwrap();
        assert_eq!(row.task, TaskKind::SemanticSearch);
        assert!(reg.is_canonical(&ModelId::from("mobileclip-b")));
        assert!(!reg.is_canonical(&ModelId::from("not-a-real-model")));
    }

    #[test]
    fn embedding_tasks_declare_dim_and_metric_detection_tasks_do_not() {
        let reg = Registry::canonical();
        assert_eq!(
            reg.dim_for(TaskKind::SemanticSearch),
            Some(EmbeddingDim(512))
        );
        assert_eq!(
            reg.metric_for(TaskKind::SemanticSearch),
            Some(DistanceMetric::Cosine)
        );
        assert_eq!(
            reg.dim_for(TaskKind::FaceRecognition),
            Some(EmbeddingDim(512))
        );
        // Detection tasks have no stored-embedding spec.
        assert_eq!(reg.dim_for(TaskKind::ObjectDetection), None);
        assert_eq!(reg.dim_for(TaskKind::FaceDetection), None);
    }

    #[test]
    fn check_insert_accepts_current_canonical_only() {
        let reg = Registry::canonical();
        // Current canonical pair → accepted with its (dim, metric).
        assert_eq!(
            reg.check_insert(
                TaskKind::SemanticSearch,
                &ModelId::from("mobileclip-b"),
                &ModelVersion::from("1"),
            ),
            Ok((EmbeddingDim(512), DistanceMetric::Cosine))
        );
        // Non-canonical model id → NonCanonical.
        assert_eq!(
            reg.check_insert(
                TaskKind::SemanticSearch,
                &ModelId::from("siglip-tiny"),
                &ModelVersion::from("1"),
            ),
            Err(RegistryError::NonCanonical {
                task: TaskKind::SemanticSearch,
                model_id: ModelId::from("siglip-tiny"),
            })
        );
        // Canonical model, wrong (old) version → Stale.
        assert_eq!(
            reg.check_insert(
                TaskKind::SemanticSearch,
                &ModelId::from("mobileclip-b"),
                &ModelVersion::from("0"),
            ),
            Err(RegistryError::Stale {
                task: TaskKind::SemanticSearch,
                model_id: ModelId::from("mobileclip-b"),
                version: ModelVersion::from("0"),
                canonical: ModelVersion::from("1"),
            })
        );
        // A detection task never accepts a stored embedding.
        assert_eq!(
            reg.check_insert(
                TaskKind::ObjectDetection,
                &ModelId::from("yolov10"),
                &ModelVersion::from("1"),
            ),
            Err(RegistryError::NotAnEmbeddingTask {
                task: TaskKind::ObjectDetection
            })
        );
    }

    #[test]
    fn stale_detection_tracks_the_canonical_version() {
        let reg = Registry::canonical();
        let id = ModelId::from("mobileclip-b");
        // Current pair is not stale; an older version is.
        assert!(!reg.is_stale(TaskKind::SemanticSearch, &id, &ModelVersion::from("1")));
        assert!(reg.is_stale(TaskKind::SemanticSearch, &id, &ModelVersion::from("0")));
        assert!(reg.is_current(TaskKind::SemanticSearch, &id, &ModelVersion::from("1")));
    }

    #[test]
    fn task_kind_is_a_closed_enum_with_kebab_wire_strings() {
        // Round-trip the wire string.
        let bytes = cbor::to_canonical_vec(&TaskKind::SemanticSearch).unwrap();
        let as_text: String = cbor::from_slice(&bytes).unwrap();
        assert_eq!(as_text, "semantic-search");
        let back: TaskKind = cbor::from_slice(&bytes).unwrap();
        assert_eq!(back, TaskKind::SemanticSearch);
        // An unknown task value is rejected (not "ignored as future").
        let unknown = cbor::to_canonical_vec(&"telepathy").unwrap();
        assert!(cbor::from_slice::<TaskKind>(&unknown).is_err());
    }

    #[test]
    fn provenance_tuple_round_trips_through_canonical_cbor() {
        let id = ModelId::from("adaface");
        let ver = ModelVersion::from("1");
        let id_b = cbor::to_canonical_vec(&id).unwrap();
        let ver_b = cbor::to_canonical_vec(&ver).unwrap();
        assert_eq!(cbor::from_slice::<ModelId>(&id_b).unwrap(), id);
        assert_eq!(cbor::from_slice::<ModelVersion>(&ver_b).unwrap(), ver);
        // Transparent: a ModelId encodes exactly as its string.
        assert_eq!(cbor::from_slice::<String>(&id_b).unwrap(), "adaface");
    }

    #[test]
    fn embedding_format_is_model_scoped() {
        let reg = Registry::canonical();
        let row = reg.canonical_for(TaskKind::SemanticSearch).unwrap();
        assert_eq!(row.embedding_format(), "embedding/mobileclip-b");
    }
}
