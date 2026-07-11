//! Inference orchestration — the deterministic execution path for the v1-committed slots (SSoT:
//! [AI/ML Integrations]).
//!
//! Ties a [`ModelRunner`] to the [`Workspace`]: run the canonical model over an asset's decoded
//! pixels, then land the results —
//!
//! - **embeddings** (semantic-search + face-recognition vectors) into the right `vec0` partition of
//!   the [vector index](crate::db::vector), tagged with the runner's resolved partition
//!   discriminator ([`resolve_partition`]);
//! - **zero-shot AI tags** into the asset's `tags_ai` OR-set as a **signed metadata update** through
//!   the lifecycle ([`Workspace::add_ai_tags`]) — structurally separate from user tags, mirroring
//!   [`Workspace::set_cull`]'s write-through. The semantic embeddings are reused for classification,
//!   so there is no separate classifier ([AI/ML — Image Categorization & Tagging]).
//!
//! Two invariants are enforced at this boundary:
//!
//! - **Provenance.** A runner whose declared model is not the registry canonical for a task is
//!   refused before any output is stored ([`require_canonical_runner`]).
//! - **Platform partition.** Comparable embeddings need byte-identical inference output across
//!   NPUs/CPUs. A device that reproduces the pinned known-answer bit-exactly shares the
//!   [`CANONICAL_PARTITION`]; a device that cannot is **not merged** into another platform's index —
//!   its embeddings land under its own [`platform`](ModelRunner::platform) tag and are compared only
//!   within that partition ([AI/ML — Embedding Provenance], the E2EE constraint's explicit
//!   fallback). The worst case is duplicated per-platform regeneration, never wrong results.
//!
//! Real per-platform runners ride the default-off `inference` feature; here the deterministic
//! [`FixtureRunner`](crate::ml::FixtureRunner) drives every path end-to-end with no model weights.
//!
//! [AI/ML Integrations]: https://docs/design/ai/
//! [AI/ML — Image Categorization & Tagging]: https://docs/design/ai/#image-categorization--tagging
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use thiserror::Error;
use uuid::Uuid;

use crate::db::{EmbeddingInsert, KnnHit, VectorIndexError};
use crate::lifecycle::{LifecycleError, Workspace};
use crate::ml::runner::{Embedding, Frame, ModelRunner, RunnerError};
use crate::ml::{ModelId, Registry, TaskKind};
use crate::sidecar::sidecar_v1::AiTag;

/// Failures from orchestrating inference.
#[derive(Debug, Error)]
pub enum OrchestratorError {
    /// A lifecycle write failed.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    /// The runner failed.
    #[error(transparent)]
    Runner(#[from] RunnerError),
    /// A vector-index insert / query failed.
    #[error(transparent)]
    Vector(#[from] VectorIndexError),
    /// The task has no canonical model (or is not an embedding task where one is required).
    #[error("task {task:?} has no canonical embedding model")]
    NoCanonical {
        /// The task.
        task: TaskKind,
    },
    /// The runner's declared model is not the registry canonical for the task — its outputs are
    /// refused rather than stored under a canonical tuple they did not produce.
    #[error("runner model `{declared}` is not canonical for task {task:?}")]
    NonCanonicalRunner {
        /// The task.
        task: TaskKind,
        /// The runner's declared model id.
        declared: ModelId,
    },
}

/// Confirm `runner` produces the registry's canonical `(model_id, model_version)` for `task` — the
/// embedding-provenance gate at the orchestration boundary (before any output is stored).
fn require_canonical_runner<R: ModelRunner>(
    runner: &R,
    registry: &Registry,
    task: TaskKind,
) -> Result<(), OrchestratorError> {
    let canon = registry
        .canonical_for(task)
        .ok_or(OrchestratorError::NoCanonical { task })?;
    let (declared_id, declared_ver) = runner.model(task).ok_or(RunnerError::Unsupported(task))?;
    if declared_id != canon.model_id || declared_ver != canon.canonical_version {
        return Err(OrchestratorError::NonCanonicalRunner {
            task,
            declared: declared_id,
        });
    }
    Ok(())
}

fn first_embedding(embeddings: Vec<Embedding>) -> Result<Embedding, RunnerError> {
    embeddings
        .into_iter()
        .next()
        .ok_or_else(|| RunnerError::Inference("runner returned no embedding".into()))
}

// ── Platform-partition fallback (ai.md § Embedding Provenance — the E2EE constraint) ─────────────

/// The shared partition every device that reaches bit-exactness on the deterministic execution
/// path writes into. Devices that cannot fall back to their own [`platform`](ModelRunner::platform)
/// tag and are never merged into this partition.
pub const CANONICAL_PARTITION: &str = "canonical";

/// A known-answer probe pinning the deterministic execution path for a task: a fixed input and the
/// exact embedding a bit-exact device must reproduce. Comparing a runner's output to `expected`
/// **bit-for-bit** is the byte-identical known-answer check the contract requires.
#[derive(Debug, Clone, PartialEq)]
pub struct KnownAnswer {
    /// The embedding task the probe pins.
    pub task: TaskKind,
    /// The fixed probe input bytes.
    pub input: Vec<u8>,
    /// The exact embedding a device on the canonical path must reproduce.
    pub expected: Embedding,
}

impl KnownAnswer {
    /// Capture the known-answer for `task` from a reference runner (the canonical device that
    /// defines the shared vector space). A later device compares against this via
    /// [`resolve_partition`].
    pub fn capture<R: ModelRunner>(
        runner: &R,
        registry: &Registry,
        task: TaskKind,
        input: &[u8],
    ) -> Result<Self, OrchestratorError> {
        require_canonical_runner(runner, registry, task)?;
        let expected = first_embedding(runner.embed_image(task, &[Frame::new(input)])?)?;
        Ok(Self {
            task,
            input: input.to_vec(),
            expected,
        })
    }
}

/// Resolve the `vec0` partition `runner`'s embeddings for `ka.task` must land in.
///
/// If `runner` reproduces `ka.expected` **bit-exactly** the deterministic-execution-path check
/// passed and its vectors are comparable with every other canonical device — it writes into the
/// shared [`CANONICAL_PARTITION`]. Otherwise the fallback is explicit, never silent: the runner's
/// own [`platform`](ModelRunner::platform) tag, a partition its incomparable vectors are confined to
/// and regenerated within (never merged into another platform's index).
pub fn resolve_partition<R: ModelRunner>(
    runner: &R,
    ka: &KnownAnswer,
) -> Result<String, OrchestratorError> {
    let got = first_embedding(runner.embed_image(ka.task, &[Frame::new(&ka.input)])?)?;
    Ok(if got == ka.expected {
        CANONICAL_PARTITION.to_string()
    } else {
        runner.platform().to_string()
    })
}

// ── Embedding + tagging (the deterministic execution path) ───────────────────────────────────────

/// Embed `asset_id` under `task`'s canonical model over its decoded pixels and store the vector in
/// the task's `vec0` partition under `partition` (resolve it with [`resolve_partition`]). Serves
/// both embedding slots — [`SemanticSearch`](TaskKind::SemanticSearch) and
/// [`FaceRecognition`](TaskKind::FaceRecognition). Refuses a non-canonical runner before touching
/// the index. The index is derived state, so no signed manifest is minted (recovery-first: it is
/// rebuilt by re-running inference).
pub fn embed_and_store<R: ModelRunner>(
    ws: &Workspace,
    runner: &R,
    registry: &Registry,
    asset_id: &Uuid,
    task: TaskKind,
    partition: &str,
) -> Result<(), OrchestratorError> {
    require_canonical_runner(runner, registry, task)?;
    let (model_id, model_version) = runner.model(task).ok_or(RunnerError::Unsupported(task))?;
    let bytes = ws.read_plaintext(asset_id)?;
    let embedding = first_embedding(runner.embed_image(task, &[Frame::new(&bytes)])?)?;
    ws.db().insert_embedding(
        registry,
        EmbeddingInsert {
            asset_id: &asset_id.to_string(),
            task,
            model_id: &model_id,
            model_version: &model_version,
            platform: partition,
            vector: &embedding,
        },
    )?;
    Ok(())
}

/// Zero-shot tag `asset_id`: embed the image and each candidate label with the semantic-search
/// embedder, then add as AI tags every label whose cosine similarity to the image clears
/// `threshold`. The tags land in the asset's `tags_ai` OR-set as one **signed metadata update**
/// (never in `tags_user`), each carrying the canonical `(model_id, model_version)`. Returns the
/// labels assigned. No separate classifier — the semantic embeddings are reused.
pub fn auto_tag<R: ModelRunner>(
    ws: &mut Workspace,
    runner: &R,
    registry: &Registry,
    asset_id: &Uuid,
    vocabulary: &[&str],
    threshold: f32,
) -> Result<Vec<String>, OrchestratorError> {
    let task = TaskKind::SemanticSearch;
    require_canonical_runner(runner, registry, task)?;
    let canon = registry
        .canonical_for(task)
        .ok_or(OrchestratorError::NoCanonical { task })?;

    let bytes = ws.read_plaintext(asset_id)?;
    let image = first_embedding(runner.embed_image(task, &[Frame::new(&bytes)])?)?;
    let label_vecs = runner.embed_text(vocabulary)?;

    let mut assigned = Vec::new();
    let mut ai_tags = Vec::new();
    for (label, lv) in vocabulary.iter().zip(label_vecs) {
        if cosine_sim(&image, &lv) >= threshold {
            assigned.push((*label).to_string());
            ai_tags.push(AiTag {
                tag: (*label).to_string(),
                model_id: canon.model_id.to_string(),
                model_version: canon.canonical_version.to_string(),
            });
        }
    }
    ws.add_ai_tags(asset_id, ai_tags)?;
    Ok(assigned)
}

/// Natural-language search: embed `query` with the semantic-search embedder and return the `k`
/// nearest assets in `partition`'s **current canonical** version (stale/other-partition vectors are
/// excluded structurally).
pub fn semantic_search<R: ModelRunner>(
    ws: &Workspace,
    runner: &R,
    registry: &Registry,
    query: &str,
    k: usize,
    partition: &str,
) -> Result<Vec<KnnHit>, OrchestratorError> {
    require_canonical_runner(runner, registry, TaskKind::SemanticSearch)?;
    let qv = first_embedding(runner.embed_text(&[query])?)?;
    Ok(ws
        .db()
        .knn(registry, TaskKind::SemanticSearch, &qv, k, partition)?)
}

/// Cosine similarity of two equal-length vectors (`0.0` if either is zero or lengths differ). The
/// embedders normalize their outputs, so this equals the inner product the index ranks by.
fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

// ── Device-bound execution policy (ai.md § Model Batching) ───────────────────────────────────

/// Per-asset execution mode, chosen from available memory at task start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchMode {
    /// One model resident at a time — minimizes peak VRAM at the cost of re-reading assets.
    Horizontal,
    /// All models resident per asset — minimizes I/O but risks OOM on mobile.
    Vertical,
}

/// Pick the execution mode: go vertical only when the estimated resident set fits the RAM budget;
/// otherwise stay horizontal to bound peak memory.
pub fn choose_batch_mode(ram_budget_mb: u64, models_resident_mb: u64) -> BatchMode {
    if models_resident_mb <= ram_budget_mb {
        BatchMode::Vertical
    } else {
        BatchMode::Horizontal
    }
}

/// The micro-batch sizes that keep the NPU cache hot.
pub const MICRO_BATCH_SIZES: [usize; 3] = [8, 4, 1];

/// The largest micro-batch size (from [`MICRO_BATCH_SIZES`]) that fits both the pending count and
/// the device `ceiling`. Always at least 1.
pub fn micro_batch_size(pending: usize, ceiling: usize) -> usize {
    let cap = pending.min(ceiling);
    MICRO_BATCH_SIZES
        .into_iter()
        .find(|&s| s <= cap)
        .unwrap_or(1)
}

/// Whether to pause the pipeline for heat: at or above `threshold_c` the OS may kill the app, so
/// the pipeline pauses until cooldown.
pub fn should_pause_for_heat(temp_c: f32, threshold_c: f32) -> bool {
    temp_c >= threshold_c
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::crypto::primitives::Argon2Params;
    use crate::ml::{FixtureRunner, ModelVersion};

    const PLATFORM: &str = "cpu-reference";

    fn workspace_with(lib: &TempDir, src: &TempDir, bytes: &[u8]) -> (Workspace, Uuid) {
        let img = src.path().join("p.jpg");
        std::fs::write(&img, bytes).unwrap();
        let mut ws = Workspace::create_with_params(
            lib.path(),
            b"pass",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        let album = ws.create_album("A");
        let id = ws.import_asset(album, &img).unwrap();
        (ws, id)
    }

    // ── Deterministic execution: same pixels + model version → same embedding/tags ───────────

    #[test]
    fn embed_then_semantic_search_matches_the_indexed_asset() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (ws, id) = workspace_with(&lib, &src, b"a beach at sunset");
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
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (ws, id) = workspace_with(&lib, &src, b"x");
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
            ws.db().embedding_count(TaskKind::SemanticSearch).unwrap(),
            0
        );
    }

    // ── Face + semantic vectors land in the right vec0 partitions ────────────────────────────

    #[test]
    fn semantic_and_face_vectors_land_in_separate_partitions() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (ws, id) = workspace_with(&lib, &src, b"a person on a beach");
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
            ws.db().embedding_count(TaskKind::SemanticSearch).unwrap(),
            1
        );
        assert_eq!(
            ws.db().embedding_count(TaskKind::FaceRecognition).unwrap(),
            1
        );
        let recs = ws.db().embeddings_for(&id.to_string()).unwrap();
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
            .db()
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
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (ws, id) = workspace_with(&lib, &src, b"a face crop");
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
            ws.db().embedding_count(TaskKind::FaceRecognition).unwrap(),
            1
        );
    }

    // ── Platform-partition fallback logic ────────────────────────────────────────────────────

    #[test]
    fn a_bit_exact_device_shares_the_canonical_partition() {
        let runner = FixtureRunner::new(PLATFORM);
        let reg = Registry::canonical();
        let ka = KnownAnswer::capture(
            &runner,
            &reg,
            TaskKind::SemanticSearch,
            b"known-answer probe",
        )
        .unwrap();
        // The same runner reproduces its own known-answer bit-for-bit → shared partition.
        assert_eq!(
            resolve_partition(&runner, &ka).unwrap(),
            CANONICAL_PARTITION
        );
    }

    #[test]
    fn a_non_bit_exact_device_falls_back_to_its_own_partition() {
        let runner = FixtureRunner::new("wonky-npu");
        let reg = Registry::canonical();
        // A known-answer this device's deterministic path does NOT reproduce (a synthetic vector
        // no hash-seeded embedding equals): the check fails, so the fallback is its own platform.
        let mut expected = vec![0.0f32; reg.dim_for(TaskKind::SemanticSearch).unwrap().get()];
        expected[0] = 1.0;
        let ka = KnownAnswer {
            task: TaskKind::SemanticSearch,
            input: b"known-answer probe".to_vec(),
            expected,
        };
        assert_eq!(resolve_partition(&runner, &ka).unwrap(), "wonky-npu");
    }

    #[test]
    fn fallback_partition_vectors_are_not_merged_into_the_canonical_partition() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (ws, id) = workspace_with(&lib, &src, b"a beach at sunset");
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
        let hits =
            semantic_search(&ws, &runner, &reg, "a beach at sunset", 5, "wonky-npu").unwrap();
        assert_eq!(hits[0].asset_id, id.to_string());
    }

    // ── tags_ai population: signed metadata update, structural namespace separation ──────────

    #[test]
    fn auto_tag_lands_matching_labels_in_the_ai_namespace_only() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"beach");
        let runner = FixtureRunner::new(PLATFORM);
        let reg = Registry::canonical();

        // "beach" image content embeds identically to the "beach" label; others are unrelated.
        let assigned =
            auto_tag(&mut ws, &runner, &reg, &id, &["beach", "city", "dog"], 0.99).unwrap();
        assert_eq!(assigned, vec!["beach".to_string()]);

        // It landed in the AI namespace with the canonical provenance tuple — not in user tags.
        let ai = ws.ai_tags(&id).unwrap();
        assert_eq!(ai.len(), 1);
        assert_eq!(ai[0].1.tag, "beach");
        assert_eq!(ai[0].1.model_id, "mobileclip-b");
        assert_eq!(ai[0].1.model_version, "1");
    }

    #[test]
    fn auto_tag_with_no_matches_writes_nothing() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"a mountain range");
        let runner = FixtureRunner::new(PLATFORM);
        let reg = Registry::canonical();
        let assigned = auto_tag(&mut ws, &runner, &reg, &id, &["beach", "city"], 0.99).unwrap();
        assert!(assigned.is_empty());
        assert!(ws.ai_tags(&id).unwrap().is_empty());
    }

    // ── Device-bound execution policy ────────────────────────────────────────────────────────

    #[test]
    fn batch_mode_follows_the_memory_budget() {
        assert_eq!(choose_batch_mode(4096, 2048), BatchMode::Vertical);
        assert_eq!(choose_batch_mode(1024, 2048), BatchMode::Horizontal);
        assert_eq!(choose_batch_mode(2048, 2048), BatchMode::Vertical); // exactly fits
    }

    #[test]
    fn micro_batch_clamps_to_ceiling_and_pending() {
        assert_eq!(micro_batch_size(100, 8), 8);
        assert_eq!(micro_batch_size(5, 8), 4);
        assert_eq!(micro_batch_size(3, 8), 1);
        assert_eq!(micro_batch_size(100, 4), 4);
        assert_eq!(micro_batch_size(0, 8), 1); // never zero
    }

    #[test]
    fn thermal_pause_triggers_at_threshold() {
        assert!(!should_pause_for_heat(38.0, 40.0));
        assert!(should_pause_for_heat(40.0, 40.0));
        assert!(should_pause_for_heat(41.5, 40.0));
    }
}
