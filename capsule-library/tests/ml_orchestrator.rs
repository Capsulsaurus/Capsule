//! The ML orchestration driven end to end over the library's SQLite vector index: the
//! `capsule_core::ml` seams with the `capsule-library` catalog as the `VectorIndex`.

use std::collections::BTreeMap;

use capsule_core::ml::*;
use capsule_core::sidecar::AiTag;
use capsule_library::db::DatabaseDriver;
use thiserror::Error;
use uuid::Uuid;

const PLATFORM: &str = "cpu-reference";

/// An in-memory asset store — the [`AiTagSink`] double these tests drive.
///
/// `ml` must not import `lifecycle` (that was the cycle), so the orchestration is exercised
/// here against the smallest store that satisfies the seam. The production implementor is
/// `Workspace`, and `lifecycle::metadata` covers it end-to-end: that the sink lands tags in
/// the *signed sidecar* is a lifecycle guarantee, not an orchestration one.
struct MemStore {
    db: DatabaseDriver,
    bytes: BTreeMap<Uuid, Vec<u8>>,
    tags: BTreeMap<Uuid, Vec<AiTag>>,
}

/// The store's read failure — the concrete error the seam type-erases.
#[derive(Debug, Error)]
#[error("no such asset: {0}")]
struct MissingAsset(Uuid);

impl MemStore {
    /// A store holding one asset with `bytes` as its plaintext; returns its id.
    fn with_asset(bytes: &[u8]) -> (Self, Uuid) {
        let id = Uuid::now_v7();
        let store = Self {
            db: DatabaseDriver::open_in_memory().expect("an in-memory catalog opens"),
            bytes: [(id, bytes.to_vec())].into_iter().collect(),
            tags: BTreeMap::new(),
        };
        (store, id)
    }

    /// The AI tags recorded for `asset_id`.
    fn ai_tags(&self, asset_id: &Uuid) -> &[AiTag] {
        self.tags.get(asset_id).map_or(&[], Vec::as_slice)
    }
}

impl AssetSource for MemStore {
    type Error = MissingAsset;

    fn read_plaintext(&self, asset_id: &Uuid) -> Result<Vec<u8>, MissingAsset> {
        self.bytes
            .get(asset_id)
            .cloned()
            .ok_or(MissingAsset(*asset_id))
    }

    type Index = DatabaseDriver;

    fn vector_index(&self) -> &DatabaseDriver {
        &self.db
    }
}

impl AiTagSink for MemStore {
    fn add_ai_tags(&mut self, asset_id: &Uuid, tags: Vec<AiTag>) -> Result<(), MissingAsset> {
        if !self.bytes.contains_key(asset_id) {
            return Err(MissingAsset(*asset_id));
        }
        self.tags.entry(*asset_id).or_default().extend(tags);
        Ok(())
    }
}

// ── Deterministic execution: same pixels + model version → same embedding/tags ───────────

#[test]
fn embed_then_semantic_search_matches_the_indexed_asset() {
    let (ws, id) = MemStore::with_asset(b"a beach at sunset");
    let runner = FixtureRunner::new(PLATFORM);
    let reg = Registry::canonical();

    embed_and_store(
        &ws,
        &runner,
        &reg,
        &id,
        TaskKind::SemanticSearch,
        CANONICAL_PARTITION,
    )
    .unwrap();

    // The asset's content embeds identically to the matching query text → it is the top hit.
    let hits = semantic_search(
        &ws,
        &runner,
        &reg,
        "a beach at sunset",
        5,
        CANONICAL_PARTITION,
    )
    .unwrap();
    assert_eq!(hits[0].asset_id, id.to_string());
    assert!(hits[0].distance < 1e-4);
    // An unrelated query does not match it at distance ~0.
    let other =
        semantic_search(&ws, &runner, &reg, "a city street", 5, CANONICAL_PARTITION).unwrap();
    assert!(other.iter().all(|h| h.distance > 1e-3));
}

#[test]
fn embed_and_store_refuses_a_non_canonical_runner() {
    let (ws, id) = MemStore::with_asset(b"x");
    // The runner declares model version 2, but the live registry's canonical is version 1.
    let mut bumped = Registry::canonical();
    bumped.set_canonical_version(TaskKind::SemanticSearch, ModelVersion::from("2"));
    let runner = FixtureRunner::with_registry(PLATFORM, bumped);
    let reg = Registry::canonical();

    let err = embed_and_store(
        &ws,
        &runner,
        &reg,
        &id,
        TaskKind::SemanticSearch,
        CANONICAL_PARTITION,
    )
    .unwrap_err();
    assert!(matches!(err, OrchestratorError::NonCanonicalRunner { .. }));
    // Nothing was stored.
    assert_eq!(
        ws.vector_index()
            .embedding_count(TaskKind::SemanticSearch)
            .unwrap(),
        0
    );
}

// ── Face + semantic vectors land in the right vec0 partitions ────────────────────────────

#[test]
fn semantic_and_face_vectors_land_in_separate_partitions() {
    let (ws, id) = MemStore::with_asset(b"a person on a beach");
    let runner = FixtureRunner::new(PLATFORM);
    let reg = Registry::canonical();

    // Both embedding slots run over the same asset.
    embed_and_store(
        &ws,
        &runner,
        &reg,
        &id,
        TaskKind::SemanticSearch,
        CANONICAL_PARTITION,
    )
    .unwrap();
    embed_and_store(
        &ws,
        &runner,
        &reg,
        &id,
        TaskKind::FaceRecognition,
        CANONICAL_PARTITION,
    )
    .unwrap();

    // Each landed in its own task's vec0 table with the canonical provenance tuple.
    assert_eq!(
        ws.vector_index()
            .embedding_count(TaskKind::SemanticSearch)
            .unwrap(),
        1
    );
    assert_eq!(
        ws.vector_index()
            .embedding_count(TaskKind::FaceRecognition)
            .unwrap(),
        1
    );
    let recs = ws.vector_index().embeddings_for(&id.to_string()).unwrap();
    assert_eq!(recs.len(), 2);
    let sem = recs
        .iter()
        .find(|r| r.task == TaskKind::SemanticSearch)
        .unwrap();
    assert_eq!(sem.model_id, ModelId::from("mobileclip-b"));
    let face = recs
        .iter()
        .find(|r| r.task == TaskKind::FaceRecognition)
        .unwrap();
    assert_eq!(face.model_id, ModelId::from("adaface"));
    // A face query never draws from the semantic partition and vice versa.
    let face_q = runner
        .embed_image(
            TaskKind::FaceRecognition,
            &[Frame::new(b"a person on a beach")],
        )
        .unwrap()
        .remove(0);
    let hits = ws
        .vector_index()
        .knn(
            &reg,
            TaskKind::FaceRecognition,
            &face_q,
            5,
            CANONICAL_PARTITION,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].asset_id, id.to_string());
}

#[test]
fn embed_and_store_serves_face_recognition() {
    let (ws, id) = MemStore::with_asset(b"a face crop");
    let runner = FixtureRunner::new(PLATFORM);
    let reg = Registry::canonical();
    embed_and_store(
        &ws,
        &runner,
        &reg,
        &id,
        TaskKind::FaceRecognition,
        CANONICAL_PARTITION,
    )
    .unwrap();
    assert_eq!(
        ws.vector_index()
            .embedding_count(TaskKind::FaceRecognition)
            .unwrap(),
        1
    );
}

#[test]
fn fallback_partition_vectors_are_not_merged_into_the_canonical_partition() {
    let (ws, id) = MemStore::with_asset(b"a beach at sunset");
    let runner = FixtureRunner::new("wonky-npu");
    let reg = Registry::canonical();

    // This device could not reach bit-exactness → its vectors land in its own partition.
    embed_and_store(
        &ws,
        &runner,
        &reg,
        &id,
        TaskKind::SemanticSearch,
        "wonky-npu",
    )
    .unwrap();

    // A query in the canonical partition never sees the fallback device's vectors.
    assert!(
        semantic_search(
            &ws,
            &runner,
            &reg,
            "a beach at sunset",
            5,
            CANONICAL_PARTITION
        )
        .unwrap()
        .is_empty(),
        "incomparable per-platform vectors must not merge into the canonical index"
    );
    // …but a query within the device's own partition finds it.
    let hits = semantic_search(&ws, &runner, &reg, "a beach at sunset", 5, "wonky-npu").unwrap();
    assert_eq!(hits[0].asset_id, id.to_string());
}

// ── tags_ai population: signed metadata update, structural namespace separation ──────────

#[test]
fn auto_tag_lands_matching_labels_in_the_ai_namespace_only() {
    let (mut ws, id) = MemStore::with_asset(b"beach");
    let runner = FixtureRunner::new(PLATFORM);
    let reg = Registry::canonical();

    // "beach" image content embeds identically to the "beach" label; others are unrelated.
    let assigned = auto_tag(&mut ws, &runner, &reg, &id, &["beach", "city", "dog"], 0.99).unwrap();
    assert_eq!(assigned, vec!["beach".to_string()]);

    // It reached the sink with the canonical provenance tuple attached.
    let ai = ws.ai_tags(&id);
    assert_eq!(ai.len(), 1);
    assert_eq!(ai[0].tag, "beach");
    assert_eq!(ai[0].model_id, "mobileclip-b");
    assert_eq!(ai[0].model_version, "1");
}

#[test]
fn auto_tag_with_no_matches_writes_nothing() {
    let (mut ws, id) = MemStore::with_asset(b"a mountain range");
    let runner = FixtureRunner::new(PLATFORM);
    let reg = Registry::canonical();
    let assigned = auto_tag(&mut ws, &runner, &reg, &id, &["beach", "city"], 0.99).unwrap();
    assert!(assigned.is_empty());
    assert!(
        ws.ai_tags(&id).is_empty(),
        "an empty tag batch must not reach the sink at all"
    );
}
