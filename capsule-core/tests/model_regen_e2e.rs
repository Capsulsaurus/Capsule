//! **E2E case 10 — model regen after a model-version bump** (slice `S-Q6`).
//!
//! Contract: [Module Map — E2E Test Surface] case 10, whose normative wording is "bump the
//! canonical model version; assert stale embeddings are excluded from queries; background regen
//! produces fresh embeddings; queries return correct results afterwards". Its invariants are owned
//! by [AI/ML — Embedding Provenance]: every stored embedding carries the `(model_id,
//! model_version)` that produced it; a swap flags the prior version stale and excludes it from
//! queries; regeneration is a background per-asset **replace**, never a global truncate; and the
//! vector index is **derived state** — rebuilt by re-running inference over the originals, never
//! restored from a backup.
//!
//! The case is entirely inside `capsule-core::ml` × the `capsule-core::db` vector index, so it is
//! unaffected by the server rebuild and runs offline with no network and no fixtures on disk.
//!
//! What makes this the *end-to-end* proof rather than the loop's unit test (which lives in
//! `ml::regen`, driven by `DeterministicEmbedder`):
//!
//! - the assets are imported into a **real [`Workspace`]** — encrypted originals, signed
//!   manifests, the workspace's own catalog — so regeneration reads the originals back through
//!   the production [`AssetSource`](capsule_core::ml::AssetSource) path;
//! - indexing and regeneration go through the **same two seams** ([`ModelRunner`] over decoded
//!   bytes, [`AssetSource`](capsule_core::ml::AssetSource) for the originals), via
//!   [`embed_and_store`] then [`RunnerEmbedder`], so a regenerated vector is byte-identical to
//!   what a fresh import at the new version would produce;
//! - the partition is *resolved* by the known-answer check ([`resolve_partition`]) rather than
//!   assumed, as it is on a real device.
//!
//! Real per-platform inference rides the default-off `inference` feature; [`FixtureRunner`] is the
//! deterministic stand-in, and deliberately so — the property under test is the orchestration (a
//! bump invalidates the right rows; regeneration repopulates them), not any model's output
//! quality. **No model weights are committed to this repository.**
//!
//! [Module Map — E2E Test Surface]: https://docs/design/module-map/#e2e-test-surface
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance
#![cfg(feature = "native")]

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;
use capsule_core::ml::{
    AiTagSink, AssetSource, CANONICAL_PARTITION, FixtureRunner, Frame, KnownAnswer, ModelId,
    ModelRunner, ModelVersion, Registry, RunnerEmbedder, TaskKind, auto_tag, embed_and_store,
    regenerate_stale, resolve_partition, semantic_search,
};
use tempfile::TempDir;
use uuid::Uuid;

/// The device tag the fixture runner reports. Whether embeddings actually land under it or under
/// the shared [`CANONICAL_PARTITION`] is decided by the known-answer check, never assumed.
const DEVICE: &str = "cpu-reference";

/// The one embedding slot this case exercises.
const TASK: TaskKind = TaskKind::SemanticSearch;

/// The library's assets: distinct plaintext, so distinct content ⇒ distinct vectors, and the
/// fixture's "identical content embeds identically across image and text" property makes a
/// semantic query's expected hit unambiguous.
const ASSETS: [&str; 3] = [
    "a beach at sunset",
    "a city street at night",
    "a mountain range",
];

/// A workspace with a fast Argon2 cost (the production cost would dominate the suite) holding
/// [`ASSETS`], returned alongside `(content, asset_id)` pairs in library order.
fn library(lib: &TempDir, src: &TempDir) -> (Workspace, Vec<(&'static str, Uuid)>) {
    let mut ws = Workspace::create_with_params(
        lib.path(),
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .expect("create workspace");
    let album = ws.create_album("Case 10").expect("create album");
    let assets = ASSETS
        .iter()
        .enumerate()
        .map(|(i, content)| {
            let path = src.path().join(format!("asset-{i}.jpg"));
            std::fs::write(&path, content.as_bytes()).expect("write source");
            let id = ws.import_asset(album, &path).expect("import");
            (*content, id)
        })
        .collect();
    (ws, assets)
}

/// The partition this device's vectors belong to, decided the way a real device decides it: run
/// the pinned known-answer probe and compare bit-for-bit. A bit-exact device shares the canonical
/// partition; one that is not is confined to its own.
fn partition_for(runner: &FixtureRunner, registry: &Registry) -> String {
    let ka = KnownAnswer::capture(runner, registry, TASK, b"known-answer probe").expect("capture");
    resolve_partition(runner, &ka).expect("resolve partition")
}

/// The `(model_id, model_version)` recorded against `asset`'s stored embedding for [`TASK`] —
/// the embedding-provenance tuple. Without it, "invalidate exactly what the bump affects" is not
/// expressible: staleness *is* this tuple disagreeing with the registry's canonical row.
fn provenance_of(ws: &Workspace, asset: Uuid) -> (ModelId, ModelVersion) {
    let recs = ws
        .db()
        .embeddings_for(&asset.to_string())
        .expect("read provenance");
    let rec = recs
        .iter()
        .find(|r| r.task == TASK)
        .expect("an embedding for the task");
    (rec.model_id.clone(), rec.model_version.clone())
}

/// Index every asset at `runner`'s canonical version over its **decrypted original** — the
/// first-time indexing path (`S-H1`/`S-H3`), which regeneration must later reproduce exactly.
fn index_all(
    ws: &Workspace,
    runner: &FixtureRunner,
    registry: &Registry,
    assets: &[(&str, Uuid)],
    partition: &str,
) {
    for (_, id) in assets {
        embed_and_store(ws, runner, registry, id, TASK, partition).expect("index asset");
    }
}

/// **E2E case 10.** The whole chain: embeddings exist at model version N carrying their
/// provenance → the canonical version bumps to N+1 → every affected entry is invalidated and
/// excluded from queries → background regeneration re-runs the model over the originals →
/// the index answers correctly at the new version, with the same number of rows it had before.
#[test]
fn e2e_case_10_model_regen_after_version_bump() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, assets) = library(&lib, &src);
    let (beach, mountain) = (assets[0].1, assets[2].1);

    // ── Model version N: index the library and confirm queries answer correctly. ──────────────
    let reg_v1 = Registry::canonical();
    let runner_v1 = FixtureRunner::new(DEVICE);
    let partition = partition_for(&runner_v1, &reg_v1);
    assert_eq!(
        partition, CANONICAL_PARTITION,
        "the reference runner reproduces its own known answer, so it shares the canonical space"
    );
    index_all(&ws, &runner_v1, &reg_v1, &assets, &partition);

    // Provenance: every embedding records the model version that produced it.
    let v1 = ModelVersion::from("1");
    for (_, id) in &assets {
        assert_eq!(
            provenance_of(&ws, *id),
            (ModelId::from("mobileclip-b"), v1.clone()),
            "an embedding must record which model version produced it"
        );
    }
    let hits = semantic_search(&ws, &runner_v1, &reg_v1, ASSETS[0], 5, &partition).unwrap();
    assert_eq!(hits[0].asset_id, beach.to_string());
    assert!(hits[0].distance < 1e-4, "exact content match at version 1");

    // ── The swap: bump the canonical model version to N+1. ────────────────────────────────────
    let mut reg_v2 = Registry::canonical();
    assert_eq!(reg_v2.bump_version(TASK).unwrap(), ModelVersion::from("2"));
    let runner_v2 = FixtureRunner::with_registry(DEVICE, reg_v2.clone());

    // Invalidation is exactly "the recorded tuple no longer matches the canonical row": every
    // stored entry becomes the regeneration work-list…
    let mut expected: Vec<String> = assets.iter().map(|(_, id)| id.to_string()).collect();
    expected.sort();
    assert_eq!(
        ws.db()
            .stale_embedding_assets(&reg_v2, TASK, &partition)
            .unwrap(),
        expected,
        "the bump invalidates precisely the entries carrying the superseded version"
    );
    // …and is excluded from queries until regenerated. The v1 vectors are still physically in the
    // index — a query at the new version simply cannot reach them (never compared across spaces).
    assert!(
        semantic_search(&ws, &runner_v2, &reg_v2, ASSETS[0], 5, &partition)
            .unwrap()
            .is_empty(),
        "stale embeddings are excluded from queries until regenerated"
    );
    assert_eq!(
        ws.db().embedding_count(TASK).unwrap(),
        3,
        "excluded, not deleted: old entries go only after new ones persist"
    );
    // A client still running the retired model is refused outright rather than served a stale
    // vector space: the provenance gate sits ahead of the query.
    assert!(
        semantic_search(&ws, &runner_v1, &reg_v2, ASSETS[0], 5, &partition).is_err(),
        "a non-canonical runner may not query the current index"
    );

    // ── Background regeneration: re-run inference over the originals, per asset. ──────────────
    let embedder = RunnerEmbedder::new(&ws, &runner_v2, &partition);
    let report = regenerate_stale(embedder.index(), &reg_v2, &embedder, TASK, None).unwrap();
    assert_eq!(report.regenerated, 3);
    assert!(report.is_complete());
    assert!(
        ws.db()
            .stale_embedding_assets(&reg_v2, TASK, &partition)
            .unwrap()
            .is_empty()
    );

    // Provenance moved with the vectors, and it was a per-asset replace — not an accumulation and
    // not a truncate-and-rebuild: the row count never changed.
    for (_, id) in &assets {
        assert_eq!(
            provenance_of(&ws, *id),
            (ModelId::from("mobileclip-b"), ModelVersion::from("2"))
        );
    }
    assert_eq!(ws.db().embedding_count(TASK).unwrap(), 3);

    // The rebuild really re-ran the model over the originals: each stored vector equals inference
    // over the asset's own decrypted plaintext at the new version, and differs from the v1 vector
    // it replaced. A regeneration that copied or re-labelled the stale vector fails here.
    for (content, id) in &assets {
        let plaintext = ws.read_plaintext(id).expect("read the original back");
        assert_eq!(plaintext, content.as_bytes(), "the originals are untouched");
        let fresh = runner_v2
            .embed_image(TASK, &[Frame::new(&plaintext)])
            .unwrap()
            .remove(0);
        let stale = runner_v1
            .embed_image(TASK, &[Frame::new(&plaintext)])
            .unwrap()
            .remove(0);
        assert_ne!(fresh, stale, "a bump is a genuine re-embed, not a re-label");
        let hits = ws.db().knn(&reg_v2, TASK, &fresh, 1, &partition).unwrap();
        assert_eq!(hits[0].asset_id, id.to_string());
        assert!(
            hits[0].distance < 1e-4,
            "the stored vector is inference over the original at the new version"
        );
    }

    // ── Afterwards: queries return correct results at the new version. ────────────────────────
    let hits = semantic_search(&ws, &runner_v2, &reg_v2, ASSETS[0], 5, &partition).unwrap();
    assert_eq!(hits.len(), 3, "the whole library is queryable again");
    assert_eq!(hits[0].asset_id, beach.to_string());
    assert!(hits[0].distance < 1e-4);
    let hits = semantic_search(&ws, &runner_v2, &reg_v2, ASSETS[2], 5, &partition).unwrap();
    assert_eq!(hits[0].asset_id, mountain.to_string());
    // The retired vector space is not served: a query vector from the old model matches nothing.
    let old_query = runner_v1.embed_text(&[ASSETS[0]]).unwrap().remove(0);
    assert!(
        ws.db()
            .knn(&reg_v2, TASK, &old_query, 5, &partition)
            .unwrap()
            .iter()
            .all(|h| h.distance > 1e-3),
        "cross-version comparison stays forbidden after the rebuild"
    );
}

/// **E2E case 10, interrupted.** Regeneration is a *background* task, so it must survive being
/// stopped: it keeps no cursor, re-deriving its work-list from current staleness. Mid-rebuild the
/// index serves a consistent partial answer — already-regenerated assets are queryable, the rest
/// stay excluded — which is what "old entries are removed only after new ones persist" buys.
#[test]
fn e2e_case_10_regeneration_is_resumable_and_serves_partial_progress() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, assets) = library(&lib, &src);

    let reg_v1 = Registry::canonical();
    let runner_v1 = FixtureRunner::new(DEVICE);
    let partition = partition_for(&runner_v1, &reg_v1);
    index_all(&ws, &runner_v1, &reg_v1, &assets, &partition);

    let mut reg_v2 = Registry::canonical();
    reg_v2.bump_version(TASK);
    let runner_v2 = FixtureRunner::with_registry(DEVICE, reg_v2.clone());
    let embedder = RunnerEmbedder::new(&ws, &runner_v2, &partition);

    // Work-list order is the sorted asset ids, so a bounded chunk's effect is predictable.
    let order = ws
        .db()
        .stale_embedding_assets(&reg_v2, TASK, &partition)
        .unwrap();
    assert_eq!(order.len(), 3);

    let mut done: Vec<String> = Vec::new();
    for step in 1..=3 {
        // One asset per invocation, then "die": the next call is a cold start.
        let report = regenerate_stale(embedder.index(), &reg_v2, &embedder, TASK, Some(1)).unwrap();
        assert_eq!((report.regenerated, report.remaining), (1, 3 - step));
        done.push(order[step - 1].clone());

        // The re-derived work-list is exactly the untouched tail — no cursor, no redo, no skip.
        assert_eq!(
            ws.db()
                .stale_embedding_assets(&reg_v2, TASK, &partition)
                .unwrap(),
            order[step..].to_vec()
        );
        // And the index answers with exactly the assets rebuilt so far.
        let mut visible: Vec<String> = semantic_search(
            &ws,
            &runner_v2,
            &reg_v2,
            "a beach at sunset",
            10,
            &partition,
        )
        .unwrap()
        .into_iter()
        .map(|h| h.asset_id)
        .collect();
        visible.sort();
        let mut expected = done.clone();
        expected.sort();
        assert_eq!(
            visible, expected,
            "mid-rebuild the index serves the regenerated assets and only those"
        );
        // Never more rows than the library has assets: replace, not accumulate.
        assert_eq!(ws.db().embedding_count(TASK).unwrap(), 3);
    }

    // Draining an already-drained partition is a clean no-op — the loop reaches a fixed point.
    let again = regenerate_stale(embedder.index(), &reg_v2, &embedder, TASK, None).unwrap();
    assert_eq!((again.regenerated, again.remaining), (0, 0));
}

/// **E2E case 10, the AI-tag half.** The same bump invalidates the other derived AI output:
/// `tags_ai` entries carry the same `(model_id, model_version)`, so a superseded suggestion drops
/// out of the *current* view until it is regenerated — while the signed sidecar, which is the
/// source of truth, keeps every tag it was ever given.
#[test]
fn e2e_case_10_stale_ai_tags_are_excluded_until_regenerated() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, assets) = library(&lib, &src);
    let beach = assets[0].1;
    let vocabulary = ["a beach at sunset", "a city street at night"];

    let reg_v1 = Registry::canonical();
    let runner_v1 = FixtureRunner::new(DEVICE);
    let assigned = auto_tag(&mut ws, &runner_v1, &reg_v1, &beach, &vocabulary, 0.99).unwrap();
    assert_eq!(assigned, vec![ASSETS[0].to_string()]);
    assert_eq!(
        ws.current_ai_tags(&reg_v1, &beach)
            .unwrap()
            .into_iter()
            .map(|t| (t.tag, t.model_version))
            .collect::<Vec<_>>(),
        vec![(ASSETS[0].to_string(), "1".to_string())]
    );

    // The bump supersedes the suggestion's model version: it is no longer current.
    let mut reg_v2 = Registry::canonical();
    reg_v2.bump_version(TASK);
    let runner_v2 = FixtureRunner::with_registry(DEVICE, reg_v2.clone());
    assert!(
        ws.current_ai_tags(&reg_v2, &beach).unwrap().is_empty(),
        "AI outputs from a superseded model are excluded until regenerated"
    );

    // Re-running the tagger at the new version restores it, tagged with the new provenance…
    let assigned = auto_tag(&mut ws, &runner_v2, &reg_v2, &beach, &vocabulary, 0.99).unwrap();
    assert_eq!(assigned, vec![ASSETS[0].to_string()]);
    assert_eq!(
        ws.current_ai_tags(&reg_v2, &beach)
            .unwrap()
            .into_iter()
            .map(|t| (t.tag, t.model_version))
            .collect::<Vec<_>>(),
        vec![(ASSETS[0].to_string(), "2".to_string())]
    );
    // …and the sidecar, the source of truth, retained both provenances.
    assert_eq!(ws.ai_tags(&beach).unwrap().len(), 2);
    // The sink wrote through the signed metadata path, not a side channel.
    assert!(ws.asset(&beach).unwrap().sidecar.signature.is_some());
    assert!(
        ws.asset(&beach)
            .unwrap()
            .sidecar
            .tags_user
            .value()
            .is_empty()
    );
    // The write half of the seam is the workspace itself (kept honest as a trait use).
    AiTagSink::add_ai_tags(&mut ws, &beach, Vec::new()).unwrap();
    // The read half hands the orchestration the workspace's own catalog.
    assert_eq!(
        std::ptr::from_ref(AssetSource::vector_index(&ws)),
        std::ptr::from_ref(ws.db())
    );
}
