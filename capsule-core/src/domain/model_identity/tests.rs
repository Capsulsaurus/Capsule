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
