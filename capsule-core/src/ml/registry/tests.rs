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
