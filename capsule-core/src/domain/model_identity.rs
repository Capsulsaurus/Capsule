//! Model-identity vocabulary — the names a task, a model, and a vector space are identified by.
//!
//! These types are **shared vocabulary, not ML behaviour**. Two very different layers need them:
//!
//! - the [vector index](crate::db::vector) names its `vec0` tables after a [`TaskKind`], sizes
//!   their columns with an [`EmbeddingDim`], ranks them by a [`DistanceMetric`], and partitions
//!   them by a [`ModelVersion`] — all before any model has run;
//! - the [model registry](crate::ml::Registry) uses the same names to *describe* the models it
//!   declares.
//!
//! They used to live in `ml::registry`, which forced `db` to `use crate::ml` merely to spell its
//! own column types — closing a `db -> ml -> db` module cycle. Hoisting them into `domain` (a leaf
//! with no outgoing crate edges) turns that cycle into the DAG `domain <- db <- ml`. The behaviour
//! that reads these names — the canonical inventory, the swap primitive, and the provenance gate —
//! stays in [`crate::ml::Registry`]; only the vocabulary moved. Every name here is re-exported
//! from [`crate::ml`], so `capsule_core::ml::TaskKind` and friends resolve unchanged.
//!
//! [`RegistryError`] rides along because it *is* vocabulary: it names a task and a model id and
//! nothing else, and the vector index has to be able to spell it to carry a refusal outward.

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
/// [`ModelRow`](crate::ml::ModelRow). Serializes transparently as its string so it interoperates
/// with the `model_id` fields on [`AiTag`](crate::sidecar::sidecar_v1::AiTag) and
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
/// — this is the design's inner-product ranking intent, expressed with the metric the SQLite
/// `vec0` engine implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DistanceMetric {
    /// Cosine distance over normalized vectors (== inner-product ranking).
    Cosine,
    /// Squared Euclidean (L2) distance.
    L2,
}

/// Refusals from the embedding-provenance invariant — produced by the
/// [gate](crate::db::vector::EmbeddingProvenance) the vector index calls on every insert, and
/// carried outward by [`VectorIndexError`](crate::db::VectorIndexError).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// The `model_id` is not a known inventory model for the task (an unknown model). A stale but
    /// *known* version is **not** this error — it is admitted as a stale row.
    #[error("model `{model_id}` is not known for task {task:?}")]
    NonCanonical {
        /// The task.
        task: TaskKind,
        /// The offending model id.
        model_id: ModelId,
    },
    /// The task does not produce stored embeddings (e.g. a detection task).
    #[error("task {task:?} does not produce stored embeddings")]
    NotAnEmbeddingTask {
        /// The task.
        task: TaskKind,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_all_lists_every_variant_once() {
        // A missed variant here would silently drop a task from every inventory walk.
        let mut seen = TaskKind::ALL.to_vec();
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n, "TaskKind::ALL must not repeat a variant");
        // The exhaustive match below fails to compile if a variant is added without listing it.
        for task in TaskKind::ALL {
            let listed = match task {
                TaskKind::SemanticSearch
                | TaskKind::ObjectDetection
                | TaskKind::FaceDetection
                | TaskKind::FaceRecognition => true,
            };
            assert!(listed);
        }
    }

    #[test]
    fn task_kind_serializes_as_its_kebab_wire_string() {
        assert_eq!(
            serde_json::to_string(&TaskKind::SemanticSearch).unwrap(),
            "\"semantic-search\""
        );
        assert_eq!(
            serde_json::to_string(&TaskKind::FaceRecognition).unwrap(),
            "\"face-recognition\""
        );
        // Closed enum: an unknown value is a structural error, not a future value to ignore.
        assert!(serde_json::from_str::<TaskKind>("\"telepathy\"").is_err());
    }

    #[test]
    fn model_newtypes_are_transparent_strings() {
        let id = ModelId::from("mobileclip-b");
        assert_eq!(id.as_str(), "mobileclip-b");
        assert_eq!(id.to_string(), "mobileclip-b");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"mobileclip-b\"");
        assert_eq!(ModelId::from("mobileclip-b".to_string()), id);

        let ver = ModelVersion::from("1");
        assert_eq!(ver.as_str(), "1");
        assert_eq!(ver.to_string(), "1");
        assert_eq!(serde_json::to_string(&ver).unwrap(), "\"1\"");
    }

    #[test]
    fn embedding_dim_reports_its_buffer_size() {
        assert_eq!(EmbeddingDim(512).get(), 512);
        assert_eq!(EmbeddingDim(512).to_string(), "512");
        // Ordering is by dimension (used to compare a declared dim against a vector length).
        assert!(EmbeddingDim(128) < EmbeddingDim(512));
    }

    #[test]
    fn distance_metric_serializes_as_its_kebab_wire_string() {
        assert_eq!(
            serde_json::to_string(&DistanceMetric::Cosine).unwrap(),
            "\"cosine\""
        );
        assert_eq!(
            serde_json::to_string(&DistanceMetric::L2).unwrap(),
            "\"l2\""
        );
    }

    #[test]
    fn registry_error_messages_name_the_task_and_model() {
        let unknown = RegistryError::NonCanonical {
            task: TaskKind::SemanticSearch,
            model_id: ModelId::from("siglip-tiny"),
        };
        assert_eq!(
            unknown.to_string(),
            "model `siglip-tiny` is not known for task SemanticSearch"
        );
        let not_embedding = RegistryError::NotAnEmbeddingTask {
            task: TaskKind::ObjectDetection,
        };
        assert_eq!(
            not_embedding.to_string(),
            "task ObjectDetection does not produce stored embeddings"
        );
    }
}
