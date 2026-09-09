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
//! [`crate::ml::regen`], and the provenance gate the [vector index](crate::db::EmbeddingInsert) calls is
//! [`Registry::check_insert`].
//!
//! [AI/ML — Models and Algorithms]: https://docs/design/ai/#models-and-algorithms
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use crate::db::vector::{EmbeddingProvenance, VectorTableSpec};
// The identity vocabulary these rows are written in lives in the `domain` leaf — `db` names its
// `vec0` columns with the same types, and importing them from `ml` closed a `db -> ml -> db`
// cycle. Re-exported so `crate::ml::registry::TaskKind` (and every other spelling) still resolves.
pub use crate::domain::model_identity::{
    DistanceMetric, EmbeddingDim, ModelId, ModelVersion, RegistryError, TaskKind,
};

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
    /// — the value carried in [`DerivativeCore::format`](crate::crypto::provenance::manifest::DerivativeCore::format).
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
    /// until [regenerated](crate::ml::regen::regenerate_stale) from the originals.
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
    /// its regeneration half is [`regen::regenerate_stale`](crate::ml::regen::regenerate_stale),
    /// which walks the now-stale entries [`stale_embedding_assets`](crate::db::DatabaseDriver::stale_embedding_assets)
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
/// [`DatabaseDriver::create_vector_tables`](crate::db::DatabaseDriver::create_vector_tables) to
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
    fn check_insert_refuses_unknown_model_admits_known_version() {
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
        // Unknown model id → NonCanonical (refused at the insert API).
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
        // Known model, superseded (old) version → ADMITTED (not refused); it will be stale-flagged.
        assert_eq!(
            reg.check_insert(
                TaskKind::SemanticSearch,
                &ModelId::from("mobileclip-b"),
                &ModelVersion::from("0"),
            ),
            Ok((EmbeddingDim(512), DistanceMetric::Cosine))
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

    #[test]
    fn canonical_rows_match_the_docs_committed_slots() {
        // The four v1-committed slots, verbatim from ai.md § v1-Committed Slots: the model id, its
        // display label, output kind, and the named fallback the contract keeps for the slot.
        let reg = Registry::canonical();
        // Exactly the four committed slots — no more.
        assert_eq!(reg.rows().len(), 4);

        let sem = reg.canonical_for(TaskKind::SemanticSearch).unwrap();
        assert_eq!(sem.model_id, ModelId::from("mobileclip-b"));
        assert_eq!(sem.display_name, "MobileCLIP-B");
        assert_eq!(
            sem.output,
            TaskOutput::Embedding {
                dim: EmbeddingDim(512),
                metric: DistanceMetric::Cosine,
            }
        );
        // Semantic Search is the only slot the contract gives a fallback (quantized SigLIP-tiny).
        assert_eq!(sem.fallback, Some("quantized SigLIP-tiny"));
        assert!(sem.function.contains("natural-language"));

        let obj = reg.canonical_for(TaskKind::ObjectDetection).unwrap();
        assert_eq!(obj.model_id, ModelId::from("yolov10"));
        assert_eq!(obj.display_name, "YOLOv10");
        assert_eq!(obj.output, TaskOutput::Detection);
        assert_eq!(obj.fallback, None);
        assert!(obj.function.contains("person detection"));

        let fdet = reg.canonical_for(TaskKind::FaceDetection).unwrap();
        assert_eq!(fdet.model_id, ModelId::from("scrfd"));
        assert_eq!(fdet.display_name, "SCRFD");
        assert_eq!(fdet.output, TaskOutput::Detection);
        assert_eq!(fdet.fallback, None);

        let frec = reg.canonical_for(TaskKind::FaceRecognition).unwrap();
        assert_eq!(frec.model_id, ModelId::from("adaface"));
        assert_eq!(frec.display_name, "InsightFace (AdaFace)");
        assert_eq!(
            frec.output,
            TaskOutput::Embedding {
                dim: EmbeddingDim(512),
                metric: DistanceMetric::Cosine,
            }
        );
        assert_eq!(frec.fallback, None);

        // Only the two embedding slots declare a stored-vector spec.
        let embedding_slots = reg
            .rows()
            .iter()
            .filter(|r| r.embedding_spec().is_some())
            .count();
        assert_eq!(embedding_slots, 2);
    }

    #[test]
    fn bump_version_increments_the_canonical_generation_and_flags_prior_stale() {
        let mut reg = Registry::canonical();
        let id = ModelId::from("mobileclip-b");
        assert_eq!(
            reg.canonical_for(TaskKind::SemanticSearch)
                .unwrap()
                .canonical_version,
            ModelVersion::from("1")
        );
        // A swap increments the generation "1" → "2".
        let next = reg.bump_version(TaskKind::SemanticSearch).unwrap();
        assert_eq!(next, ModelVersion::from("2"));
        assert!(reg.is_current(TaskKind::SemanticSearch, &id, &ModelVersion::from("2")));
        // Everything at the prior generation is now stale.
        assert!(reg.is_stale(TaskKind::SemanticSearch, &id, &ModelVersion::from("1")));
        // Bumping again advances monotonically; each other slot bumps independently.
        assert_eq!(
            reg.bump_version(TaskKind::SemanticSearch).unwrap(),
            ModelVersion::from("3")
        );
        assert_eq!(
            reg.canonical_for(TaskKind::ObjectDetection)
                .unwrap()
                .canonical_version,
            ModelVersion::from("1"),
            "a bump on one slot must not touch another"
        );
    }

    #[test]
    fn vector_tables_describe_exactly_the_embedding_slots() {
        // The db-side schema description: one partition table per embedding slot, sized and
        // ranked from the same row the provenance gate reads. Detection slots store no vectors.
        let reg = Registry::canonical();
        let specs = reg.vector_tables();
        assert_eq!(specs.len(), 2);
        let sem = specs
            .iter()
            .find(|s| s.task == TaskKind::SemanticSearch)
            .unwrap();
        assert_eq!(sem.dim, EmbeddingDim(512));
        assert_eq!(sem.metric, DistanceMetric::Cosine);
        assert!(
            specs
                .iter()
                .any(|s| s.task == TaskKind::FaceRecognition && s.dim == EmbeddingDim(512))
        );
        assert!(
            !specs
                .iter()
                .any(|s| matches!(s.task, TaskKind::ObjectDetection | TaskKind::FaceDetection)),
            "a detection slot must not claim a vector partition"
        );
    }

    #[test]
    fn the_provenance_seam_answers_exactly_as_the_inventory_does() {
        // The vector index sees the registry only through `EmbeddingProvenance`. If that view
        // ever drifted from the inherent methods, `db` would gate on different facts than `ml`.
        let mut reg = Registry::canonical();
        reg.set_canonical_version(TaskKind::FaceRecognition, ModelVersion::from("7"));
        for task in TaskKind::ALL {
            let row = reg.canonical_for(task);
            assert_eq!(
                EmbeddingProvenance::embedding_spec(&reg, task),
                row.and_then(ModelRow::embedding_spec),
                "embedding_spec drifted for {task:?}"
            );
            assert_eq!(
                EmbeddingProvenance::canonical_version(&reg, task),
                row.map(|r| r.canonical_version.clone()),
                "canonical_version drifted for {task:?}"
            );
            let id = row.map_or_else(|| ModelId::from("none"), |r| r.model_id.clone());
            let ver = ModelVersion::from("1");
            assert_eq!(
                EmbeddingProvenance::check_insert(&reg, task, &id, &ver),
                reg.check_insert(task, &id, &ver),
                "check_insert drifted for {task:?}"
            );
        }
        // The bumped slot's partition follows the swap, so the index queries the new version.
        assert_eq!(
            EmbeddingProvenance::canonical_version(&reg, TaskKind::FaceRecognition),
            Some(ModelVersion::from("7"))
        );
    }

    #[test]
    fn bump_version_suffixes_a_non_numeric_generation() {
        let mut reg = Registry::canonical();
        reg.set_canonical_version(TaskKind::SemanticSearch, ModelVersion::from("2024-06"));
        // A non-integer version still changes on a swap (the tuple must differ).
        let next = reg.bump_version(TaskKind::SemanticSearch).unwrap();
        assert_eq!(next, ModelVersion::from("2024-06+1"));
    }
}
