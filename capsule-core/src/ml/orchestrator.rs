//! Inference orchestration (SSoT: [AI/ML Integrations]).
//!
//! Ties a [`ModelRunner`] to the [`Workspace`]: run the canonical model over an asset, then write
//! the results back through the signed lifecycle — embeddings as [embedding
//! derivatives](Workspace::add_embedding_derivative), zero-shot tags as
//! [AI tags](Workspace::add_ai_tags). Embeddings are reused for zero-shot classification, so no
//! separate classifier is needed. Also the device-bound execution policy: horizontal vs. vertical
//! batching by RAM, the micro-batch ceiling, and the thermal-throttle pause.
//!
//! The embedding-provenance invariant is enforced at this boundary: a runner whose declared model
//! is not the registry canonical for a task is refused before any output is stored.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

use thiserror::Error;
use uuid::Uuid;

use crate::db::KnnHit;
use crate::lifecycle::{LifecycleError, Workspace};
use crate::ml::reid::cosine_sim;
use crate::ml::runner::{Frame, ModelRunner, RunnerError};
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
    /// A vector-index query failed.
    #[error(transparent)]
    Vector(#[from] crate::db::VectorIndexError),
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

/// Confirm `runner` produces the registry's canonical `(model_id, model_version)` for `task`.
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

/// Embed `asset_id` under `task`'s canonical model and store the signed embedding derivative +
/// index entry. Refuses a non-canonical runner.
pub fn embed_and_store<R: ModelRunner>(
    ws: &mut Workspace,
    runner: &R,
    registry: &Registry,
    asset_id: &Uuid,
    task: TaskKind,
) -> Result<(), OrchestratorError> {
    require_canonical_runner(runner, registry, task)?;
    let bytes = ws.read_plaintext(asset_id)?;
    let embedding = runner
        .embed_image(task, &[Frame::new(&bytes)])?
        .into_iter()
        .next()
        .ok_or_else(|| RunnerError::Inference("runner returned no embedding".into()))?;
    ws.add_embedding_derivative(registry, asset_id, task, runner.platform(), &embedding)?;
    Ok(())
}

/// Zero-shot tag `asset_id`: embed the image and each candidate label with the semantic-search
/// embedder, then add as AI tags every label whose cosine similarity to the image clears
/// `threshold`. Returns the labels assigned. No separate classifier — the semantic embeddings are
/// reused (ai.md § Image Categorization & Tagging).
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
    let image = runner
        .embed_image(task, &[Frame::new(&bytes)])?
        .into_iter()
        .next()
        .ok_or_else(|| RunnerError::Inference("runner returned no embedding".into()))?;
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
    if !ai_tags.is_empty() {
        ws.add_ai_tags(asset_id, ai_tags)?;
    }
    Ok(assigned)
}

/// Natural-language search: embed `query` with the semantic-search embedder and return the `k`
/// nearest assets in the runner's platform partition (current canonical version only).
pub fn semantic_search<R: ModelRunner>(
    ws: &Workspace,
    runner: &R,
    registry: &Registry,
    query: &str,
    k: usize,
) -> Result<Vec<KnnHit>, OrchestratorError> {
    require_canonical_runner(runner, registry, TaskKind::SemanticSearch)?;
    let qv = runner
        .embed_text(&[query])?
        .into_iter()
        .next()
        .ok_or_else(|| RunnerError::Inference("runner returned no embedding".into()))?;
    Ok(ws.db().knn(
        registry,
        TaskKind::SemanticSearch,
        &qv,
        k,
        runner.platform(),
    )?)
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
    use super::*;

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

    // ── Pipeline smoke tests (orchestrator × real Workspace × FixtureRunner) ─────────────────

    use crate::crypto::primitives::Argon2Params;
    use crate::ml::FixtureRunner;
    use crate::ml::ModelVersion;
    use tempfile::TempDir;

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

    #[test]
    fn embed_then_semantic_search_matches_the_indexed_asset() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"a beach at sunset");
        let runner = FixtureRunner::new(PLATFORM);
        let reg = Registry::canonical();

        embed_and_store(&mut ws, &runner, &reg, &id, TaskKind::SemanticSearch).unwrap();

        // The asset's content embeds identically to the matching query text → it is the top hit.
        let hits = semantic_search(&ws, &runner, &reg, "a beach at sunset", 5).unwrap();
        assert_eq!(hits[0].asset_id, id.to_string());
        assert!(hits[0].distance < 1e-4);
        // An unrelated query does not match it at distance ~0.
        let other = semantic_search(&ws, &runner, &reg, "a city street", 5).unwrap();
        assert!(other.iter().all(|h| h.distance > 1e-3));
    }

    #[test]
    fn embed_and_store_refuses_a_non_canonical_runner() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"x");
        // The runner declares model version 2, but the live registry's canonical is version 1.
        let mut bumped = Registry::canonical();
        bumped.set_canonical_version(TaskKind::SemanticSearch, ModelVersion::from("2"));
        let runner = FixtureRunner::with_registry(PLATFORM, bumped);
        let reg = Registry::canonical();

        let err =
            embed_and_store(&mut ws, &runner, &reg, &id, TaskKind::SemanticSearch).unwrap_err();
        assert!(matches!(err, OrchestratorError::NonCanonicalRunner { .. }));
        // Nothing was stored.
        assert_eq!(
            ws.db().embedding_count(TaskKind::SemanticSearch).unwrap(),
            0
        );
    }

    #[test]
    fn auto_tag_assigns_matching_labels_as_ai_tags() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"beach");
        let runner = FixtureRunner::new(PLATFORM);
        let reg = Registry::canonical();

        // The image content "beach" embeds identically to the "beach" label; others are unrelated.
        let assigned =
            auto_tag(&mut ws, &runner, &reg, &id, &["beach", "city", "dog"], 0.99).unwrap();
        assert_eq!(assigned, vec!["beach".to_string()]);

        // It landed in the AI namespace with the canonical provenance tuple — not in user tags.
        let ai = ws.ai_tags(&id).unwrap();
        assert_eq!(ai.len(), 1);
        assert_eq!(ai[0].1.tag, "beach");
        assert_eq!(ai[0].1.model_id, "mobileclip-b");
        assert!(ws.db().tags_for(&id.to_string()).unwrap().is_empty());
    }
}
