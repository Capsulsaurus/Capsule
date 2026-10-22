//! Version-bump regeneration driven end to end over the library's SQLite vector index: the
//! `capsule_core::ml` regeneration loop with the `capsule-library` catalog as the
//! `VectorIndex`.

use std::collections::BTreeMap;

use capsule_core::ml::*;
use capsule_library::db::DatabaseDriver;
use thiserror::Error;
use uuid::Uuid;

const PLATFORM: &str = "cpu-reference";

fn sem() -> TaskKind {
    TaskKind::SemanticSearch
}

fn db() -> DatabaseDriver {
    DatabaseDriver::open_in_memory().unwrap()
}

/// Index an asset at the embedder's current canonical version (the S-H1 indexing path), so the
/// tests can seed a pre-bump index without the regeneration loop.
fn index(
    db: &DatabaseDriver,
    registry: &Registry,
    emb: &DeterministicEmbedder,
    asset_id: &str,
    task: TaskKind,
) {
    let (model_id, model_version) = emb.model(task).unwrap();
    let vector = emb.embed(asset_id, task).unwrap();
    db.insert_embedding(
        registry,
        EmbeddingInsert {
            asset_id,
            task,
            model_id: &model_id,
            model_version: &model_version,
            platform: emb.platform(),
            vector: &vector,
        },
    )
    .unwrap();
}

#[test]
fn deterministic_embedder_is_reproducible_and_version_sensitive() {
    let emb1 = DeterministicEmbedder::new(PLATFORM);
    let a1 = emb1.embed("asset-a", sem()).unwrap();
    // Reproducible: same key + version → identical vector.
    assert_eq!(a1, emb1.embed("asset-a", sem()).unwrap());
    // And normalized.
    let norm: f32 = a1.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5);
    // Version-sensitive: a bumped registry produces a different vector for the same asset.
    let mut bumped = Registry::canonical();
    bumped.bump_version(sem());
    let emb2 = DeterministicEmbedder::with_registry(PLATFORM, bumped);
    assert_ne!(a1, emb2.embed("asset-a", sem()).unwrap());
    // Query side reproduces the stored vector exactly.
    assert_eq!(a1, emb1.embed_key("asset-a", sem()));
}

#[test]
fn regenerate_is_a_no_op_when_nothing_is_stale() {
    let db = db();
    let reg = Registry::canonical();
    let emb = DeterministicEmbedder::new(PLATFORM);
    index(&db, &reg, &emb, "a", sem());
    let report = regenerate_stale(&db, &reg, &emb, sem(), None).unwrap();
    assert_eq!(
        report,
        RegenReport {
            task: sem(),
            regenerated: 0,
            remaining: 0
        }
    );
    assert!(report.is_complete());
}

#[test]
fn regenerate_refuses_a_non_canonical_embedder() {
    let db = db();
    let reg = Registry::canonical(); // canonical is version "1"
    // The embedder declares version "2" — not the live registry canonical.
    let mut bumped = Registry::canonical();
    bumped.bump_version(sem());
    let emb = DeterministicEmbedder::with_registry(PLATFORM, bumped);

    let err = regenerate_stale(&db, &reg, &emb, sem(), None).unwrap_err();
    assert!(matches!(err, RegenError::NonCanonicalEmbedder { .. }));
    // Nothing was touched.
    assert_eq!(db.embedding_count(sem()).unwrap(), 0);
}

#[test]
fn regenerate_rejects_a_detection_task() {
    let db = db();
    let reg = Registry::canonical();
    let emb = DeterministicEmbedder::new(PLATFORM);
    let err = regenerate_stale(&db, &reg, &emb, TaskKind::ObjectDetection, None).unwrap_err();
    assert!(matches!(err, RegenError::NotAnEmbeddingTask { .. }));
}

/// The in-module *orchestration* shape of Module Map **E2E case 10** ("Model regen after
/// version bump"), driven by the deterministic embedder: bump the canonical model version →
/// stale embeddings excluded from queries → regen produces fresh embeddings per-asset →
/// queries return correct results post-regen. The end-to-end proof — a real `Workspace`, real
/// originals, inference re-run through the [`RunnerEmbedder`] seam — is
/// `capsule-core/tests/model_regen_e2e.rs`; this one pins the loop's own bookkeeping.
#[test]
fn e2e_model_regen_after_version_bump_serves_only_current() {
    let db = db();

    // ── v1: index two assets and confirm semantic queries match the right asset. ──
    let reg1 = Registry::canonical();
    let emb1 = DeterministicEmbedder::new(PLATFORM);
    index(&db, &reg1, &emb1, "a", sem());
    index(&db, &reg1, &emb1, "b", sem());
    let hits = db
        .knn(&reg1, sem(), &emb1.embed_key("a", sem()), 5, PLATFORM)
        .unwrap();
    assert_eq!(hits[0].asset_id, "a");
    assert!(hits[0].distance < 1e-4, "exact self-match at v1");

    // ── Swap the model: bump the canonical version. Both v1 embeddings are now stale. ──
    let mut reg2 = Registry::canonical();
    assert_eq!(reg2.bump_version(sem()).unwrap(), ModelVersion::from("2"));
    let emb2 = DeterministicEmbedder::with_registry(PLATFORM, reg2.clone());

    // Pre-regen: the v1 embeddings are the regeneration work-list and are excluded from v2
    // queries (different partition) — even a query geometrically on top of them returns empty.
    assert_eq!(
        db.stale_embedding_assets(&reg2, sem(), PLATFORM).unwrap(),
        vec!["a".to_string(), "b".to_string()]
    );
    assert!(
        db.knn(&reg2, sem(), &emb2.embed_key("a", sem()), 5, PLATFORM)
            .unwrap()
            .is_empty(),
        "stale embeddings are excluded from queries until regenerated"
    );

    // ── Background regen: fresh v2 embeddings, per-asset replace (not accumulate). ──
    let report = regenerate_stale(&db, &reg2, &emb2, sem(), None).unwrap();
    assert_eq!(report.regenerated, 2);
    assert!(report.is_complete());
    assert!(
        db.stale_embedding_assets(&reg2, sem(), PLATFORM)
            .unwrap()
            .is_empty()
    );
    // Replace, not accumulate: still exactly two stored embeddings, both at v2.
    assert_eq!(db.embedding_count(sem()).unwrap(), 2);
    for asset in ["a", "b"] {
        assert_eq!(
            db.embeddings_for(asset).unwrap()[0].model_version,
            ModelVersion::from("2")
        );
    }

    // ── Post-regen: queries serve only the current version, and correctly. ──
    let hits = db
        .knn(&reg2, sem(), &emb2.embed_key("a", sem()), 5, PLATFORM)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].asset_id, "a");
    assert!(hits[0].distance < 1e-4, "exact self-match at v2 post-regen");
    // A query built from the *old* v1 vector no longer matches anything at the current version.
    assert!(
        db.knn(&reg2, sem(), &emb1.embed_key("a", sem()), 5, PLATFORM)
            .unwrap()
            .first()
            .is_none_or(|h| h.distance > 1e-3),
        "the retired v1 vector space is not served by current queries"
    );
}

/// Resumability proof: the loop persists no cursor. Regenerate in single-asset chunks,
/// simulating a process killed after each step; every restart re-derives the work-list from
/// current staleness and continues from exactly where it left off, never redoing a completed
/// asset nor skipping a pending one.
#[test]
fn regeneration_is_resumable_by_rederiving_the_worklist() {
    let db = db();
    let reg1 = Registry::canonical();
    let emb1 = DeterministicEmbedder::new(PLATFORM);
    for asset in ["a", "b", "c"] {
        index(&db, &reg1, &emb1, asset, sem());
    }

    let mut reg2 = Registry::canonical();
    reg2.bump_version(sem());
    let emb2 = DeterministicEmbedder::with_registry(PLATFORM, reg2.clone());

    // All three are stale after the swap.
    assert_eq!(
        db.stale_embedding_assets(&reg2, sem(), PLATFORM).unwrap(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );

    // Step 1 (budget 1): rebuilds "a" (sorted-first); "b","c" remain. "kill" the process here.
    let r1 = regenerate_stale(&db, &reg2, &emb2, sem(), Some(1)).unwrap();
    assert_eq!((r1.regenerated, r1.remaining), (1, 2));
    assert_eq!(
        db.stale_embedding_assets(&reg2, sem(), PLATFORM).unwrap(),
        vec!["b".to_string(), "c".to_string()],
        "the completed asset drops off the re-derived work-list"
    );
    // Replace semantics held: count never grew.
    assert_eq!(db.embedding_count(sem()).unwrap(), 3);

    // Step 2 (fresh call, budget 1): re-derives the remainder and rebuilds "b".
    let r2 = regenerate_stale(&db, &reg2, &emb2, sem(), Some(1)).unwrap();
    assert_eq!((r2.regenerated, r2.remaining), (1, 1));
    assert_eq!(
        db.stale_embedding_assets(&reg2, sem(), PLATFORM).unwrap(),
        vec!["c".to_string()]
    );

    // Step 3 (drain): rebuilds "c" and completes.
    let r3 = regenerate_stale(&db, &reg2, &emb2, sem(), None).unwrap();
    assert_eq!((r3.regenerated, r3.remaining), (1, 0));
    assert!(r3.is_complete());
    assert!(
        db.stale_embedding_assets(&reg2, sem(), PLATFORM)
            .unwrap()
            .is_empty()
    );

    // Every asset now serves at the current version, exactly once each.
    assert_eq!(db.embedding_count(sem()).unwrap(), 3);
    for asset in ["a", "b", "c"] {
        let hits = db
            .knn(&reg2, sem(), &emb2.embed_key(asset, sem()), 1, PLATFORM)
            .unwrap();
        assert_eq!(hits[0].asset_id, asset);
        assert!(hits[0].distance < 1e-4);
    }
}

#[test]
fn regenerate_running_the_whole_list_again_is_idempotent() {
    // Draining, then calling once more, is a clean no-op — the loop reaches a fixed point.
    let db = db();
    let reg1 = Registry::canonical();
    let emb1 = DeterministicEmbedder::new(PLATFORM);
    index(&db, &reg1, &emb1, "a", sem());

    let mut reg2 = Registry::canonical();
    reg2.bump_version(sem());
    let emb2 = DeterministicEmbedder::with_registry(PLATFORM, reg2.clone());

    assert_eq!(
        regenerate_stale(&db, &reg2, &emb2, sem(), None)
            .unwrap()
            .regenerated,
        1
    );
    let again = regenerate_stale(&db, &reg2, &emb2, sem(), None).unwrap();
    assert_eq!((again.regenerated, again.remaining), (0, 0));
}

// ── `RunnerEmbedder`: regeneration re-runs inference over the originals ───────────────────

/// The smallest store satisfying [`AssetSource`] — the originals the derived index is
/// rebuilt from. (`ml` must not import `lifecycle`; the `Workspace` implementor is driven
/// end-to-end by `tests/model_regen_e2e.rs`.)
struct MemStore {
    db: DatabaseDriver,
    bytes: BTreeMap<Uuid, Vec<u8>>,
}

/// The store's read failure — the concrete error the seam type-erases.
#[derive(Debug, Error)]
#[error("no such asset: {0}")]
struct MissingAsset(Uuid);

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

impl MemStore {
    fn with_assets(assets: &[(Uuid, &[u8])]) -> Self {
        Self {
            db: DatabaseDriver::open_in_memory().unwrap(),
            bytes: assets.iter().map(|(id, b)| (*id, b.to_vec())).collect(),
        }
    }
}

/// Seed a stale (v1) row for `asset` so the bump has something to invalidate.
fn seed_v1(store: &MemStore, asset: Uuid) {
    let reg1 = Registry::canonical();
    let runner1 = FixtureRunner::new(PLATFORM);
    let emb = RunnerEmbedder::new(store, &runner1, CANONICAL_PARTITION);
    let (model_id, model_version) = emb.model(sem()).unwrap();
    let vector = emb.embed(&asset.to_string(), sem()).unwrap();
    store
        .db
        .insert_embedding(
            &reg1,
            EmbeddingInsert {
                asset_id: &asset.to_string(),
                task: sem(),
                model_id: &model_id,
                model_version: &model_version,
                platform: CANONICAL_PARTITION,
                vector: &vector,
            },
        )
        .unwrap();
}

/// The whole point of the adapter: the vector it produces is inference over the asset's
/// **original bytes**, not a function of its id — so a rebuild genuinely re-runs the model.
#[test]
fn runner_embedder_embeds_the_originals_through_the_runner_seam() {
    let id = Uuid::now_v7();
    let store = MemStore::with_assets(&[(id, b"a beach at sunset")]);
    let runner = FixtureRunner::new(PLATFORM);
    let emb = RunnerEmbedder::new(&store, &runner, CANONICAL_PARTITION);

    let expected = runner
        .embed_image(sem(), &[Frame::new(b"a beach at sunset")])
        .unwrap()
        .remove(0);
    assert_eq!(emb.embed(&id.to_string(), sem()).unwrap(), expected);
    // The partition is the caller's resolved one, not the runner's device tag — a device that
    // failed the known-answer check regenerates inside its own partition.
    let fallback = FixtureRunner::new("wonky-npu");
    assert_eq!(
        RunnerEmbedder::new(&store, &fallback, "wonky-npu").platform(),
        "wonky-npu"
    );
    assert_eq!(emb.platform(), CANONICAL_PARTITION);
    // It declares the runner's pair, so the loop's canonical check applies to the real model.
    assert_eq!(emb.model(sem()), runner.model(sem()));
    // And it hands back the store's own catalog, so work-list and writes address one index.
    assert_eq!(
        std::ptr::from_ref(emb.index()),
        std::ptr::from_ref(store.vector_index())
    );
}

/// Without the originals there is nothing to regenerate from: the loop surfaces the store's
/// failure per-asset instead of writing a vector it did not derive from pixels.
#[test]
fn regeneration_fails_loudly_when_the_original_is_gone() {
    let ghost = Uuid::now_v7();
    // The index carries a stale row, but the store has lost the original.
    let store = MemStore::with_assets(&[(ghost, b"present for the v1 seed")]);
    seed_v1(&store, ghost);
    let store = MemStore {
        db: store.db,
        bytes: BTreeMap::new(),
    };

    let mut reg2 = Registry::canonical();
    reg2.bump_version(sem());
    let runner2 = FixtureRunner::with_registry(PLATFORM, reg2.clone());
    let emb = RunnerEmbedder::new(&store, &runner2, CANONICAL_PARTITION);

    let err = regenerate_stale(&store.db, &reg2, &emb, sem(), None).unwrap_err();
    let RegenError::Embed(e) = err else {
        panic!("expected a per-asset embed failure");
    };
    assert_eq!(e.asset_id, ghost.to_string());
    assert!(e.reason.contains("reading the original failed"));
    // The stale row is still stale — nothing was written under the new version.
    assert_eq!(
        store
            .db
            .stale_embedding_assets(&reg2, sem(), CANONICAL_PARTITION)
            .unwrap(),
        vec![ghost.to_string()]
    );
}

#[test]
fn runner_embedder_rejects_an_asset_id_that_is_not_a_uuid() {
    let store = MemStore::with_assets(&[]);
    let runner = FixtureRunner::new(PLATFORM);
    let emb = RunnerEmbedder::new(&store, &runner, CANONICAL_PARTITION);
    let err = emb.embed("not-a-uuid", sem()).unwrap_err();
    assert!(err.reason.contains("not a UUID"));
}
