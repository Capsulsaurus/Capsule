use super::*;
use crate::ml::ModelVersion;

#[test]
fn fixture_embeddings_are_deterministic_and_normalized() {
    let r = FixtureRunner::new("cpu-reference");
    let a = r
        .embed_image(TaskKind::SemanticSearch, &[Frame::new(b"beach")])
        .unwrap();
    let b = r
        .embed_image(TaskKind::SemanticSearch, &[Frame::new(b"beach")])
        .unwrap();
    assert_eq!(a, b, "same bytes ⇒ same vector");
    assert_eq!(a[0].len(), 512);
    let norm = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "L2-normalized");
}

#[test]
fn identical_image_and_text_content_embed_identically() {
    // The property that makes semantic-match tests possible without real inference.
    let r = FixtureRunner::new("cpu-reference");
    let img = r
        .embed_image(
            TaskKind::SemanticSearch,
            &[Frame::new(b"a beach at sunset")],
        )
        .unwrap();
    let txt = r.embed_text(&["a beach at sunset"]).unwrap();
    assert_eq!(img[0], txt[0]);
    // A different concept embeds differently.
    let other = r.embed_text(&["a city street"]).unwrap();
    assert_ne!(img[0], other[0]);
}

#[test]
fn fixture_declares_canonical_models_for_both_embedding_and_detection_slots() {
    let r = FixtureRunner::new("cpu-reference");
    assert_eq!(
        r.model(TaskKind::SemanticSearch),
        Some((ModelId::from("mobileclip-b"), ModelVersion::from("1")))
    );
    assert_eq!(
        r.model(TaskKind::FaceRecognition),
        Some((ModelId::from("adaface"), ModelVersion::from("1")))
    );
    assert_eq!(
        r.model(TaskKind::FaceDetection),
        Some((ModelId::from("scrfd"), ModelVersion::from("1")))
    );
}

#[test]
fn face_recognition_is_an_embedding_task_the_fixture_serves() {
    // Face features ride the same embedding seam as semantic search (the face slot).
    let r = FixtureRunner::new("cpu-reference");
    let v = r
        .embed_image(TaskKind::FaceRecognition, &[Frame::new(b"a face crop")])
        .unwrap();
    assert_eq!(v[0].len(), 512);
    let norm = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4);
}

#[test]
fn embedding_a_detection_task_is_unsupported() {
    let r = FixtureRunner::new("cpu-reference");
    assert_eq!(
        r.embed_image(TaskKind::ObjectDetection, &[Frame::new(b"x")]),
        Err(RunnerError::Unsupported(TaskKind::ObjectDetection))
    );
    // ...but detection on a detection task is fine (empty, deterministically).
    assert_eq!(
        r.detect(TaskKind::ObjectDetection, &[Frame::new(b"x")]),
        Ok(vec![vec![]])
    );
}

/// A model swap must change the *output*, not just the declared tag. This is the property
/// [`crate::ml::regenerate_stale`] leans on to prove regeneration re-ran inference rather than
/// re-labelling the stale vector (Module Map E2E case 10), and the property a real model swap
/// has by construction.
#[test]
fn a_version_bump_changes_the_vectors_for_unchanged_pixels() {
    let v1 = FixtureRunner::new("cpu-reference");
    let mut reg = Registry::canonical();
    reg.set_canonical_version(TaskKind::SemanticSearch, ModelVersion::from("2"));
    let v2 = FixtureRunner::with_registry("cpu-reference", reg);

    let pixels = Frame::new(b"a beach at sunset");
    let a = v1.embed_image(TaskKind::SemanticSearch, &[pixels]).unwrap();
    let b = v2.embed_image(TaskKind::SemanticSearch, &[pixels]).unwrap();
    assert_ne!(
        a, b,
        "the same pixels embed differently at a new model version"
    );
    // The text side moves with it, so image/text content equality still holds *within* v2.
    assert_eq!(b[0], v2.embed_text(&["a beach at sunset"]).unwrap()[0]);
    // …and across versions the two spaces are not comparable.
    assert_ne!(b[0], v1.embed_text(&["a beach at sunset"]).unwrap()[0]);
    // Untouched slots keep their own space: a bump on semantic search does not move faces.
    assert_eq!(
        v1.embed_image(TaskKind::FaceRecognition, &[pixels])
            .unwrap(),
        v2.embed_image(TaskKind::FaceRecognition, &[pixels])
            .unwrap()
    );
}

#[test]
fn a_bumped_registry_makes_the_fixture_declare_the_new_version() {
    let mut reg = Registry::canonical();
    reg.set_canonical_version(TaskKind::SemanticSearch, ModelVersion::from("2"));
    let r = FixtureRunner::with_registry("cpu-reference", reg);
    assert_eq!(
        r.model(TaskKind::SemanticSearch),
        Some((ModelId::from("mobileclip-b"), ModelVersion::from("2")))
    );
}
