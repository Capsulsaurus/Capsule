//! Inference orchestration — the deterministic execution path for the v1-committed slots (SSoT:
//! [AI/ML Integrations]).
//!
//! Ties a [`ModelRunner`] to an asset store: run the canonical model over an asset's decoded
//! pixels, then land the results —
//!
//! - **embeddings** (semantic-search + face-recognition vectors) into the right `vec0` partition of
//!   the [vector index](crate::ml::EmbeddingInsert), tagged with the runner's resolved partition
//!   discriminator ([`resolve_partition`]);
//! - **zero-shot AI tags** into the asset's `tags_ai` OR-set as a **signed metadata update** through
//!   the lifecycle ([`AiTagSink::add_ai_tags`]) — structurally separate from user tags, mirroring
//!   `Workspace::set_cull`'s write-through. The semantic embeddings are reused for classification,
//!   so there is no separate classifier ([AI/ML — Image Categorization & Tagging]).
//!
//! The store reaches this module through the [`AssetSource`] / [`AiTagSink`] seams rather than as a
//! concrete type. The one production implementor is
//! `Workspace` — but naming it here would make `ml` depend on
//! `lifecycle`, which already depends on `ml` (`lifecycle::metadata` reads the registry to filter
//! stale AI tags). Inverting the edge leaves the dependency running one way, and lets the
//! orchestration be driven by any store that can hand over plaintext bytes and accept AI tags.
//!
//! Two invariants are enforced at this boundary:
//!
//! - **Provenance.** A runner whose declared model is not the registry canonical for a task is
//!   refused before any output is stored (`require_canonical_runner`).
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

use crate::ml::index::{EmbeddingInsert, KnnHit, VectorIndex, VectorIndexError};
use crate::ml::{Embedding, Frame, ModelId, ModelRunner, Registry, RunnerError, TaskKind};
use crate::sidecar::AiTag;

/// The store the orchestrator reads asset pixels from and indexes vectors into.
///
/// Implemented by `Workspace`; a test double only has to produce
/// bytes for an id and hand back a catalog. Keeping it a trait is what removes the
/// `ml -> lifecycle` import (see the module docs).
pub trait AssetSource {
    /// How this store reports a failed read. Type-erased into [`StoreError`] on the way out, so
    /// `ml` never names a concrete store error.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The decoded-asset plaintext bytes inference runs over.
    fn read_plaintext(&self, asset_id: &Uuid) -> Result<Vec<u8>, Self::Error>;

    /// The vector index this store's embeddings live in.
    type Index: VectorIndex;

    /// The catalog holding this store's vector index.
    fn vector_index(&self) -> &Self::Index;
}

/// The write side: an asset store that accepts AI tags for an asset.
///
/// Separate from [`AssetSource`] because only [`auto_tag`] writes — embedding and search need a
/// shared reference, and the split keeps them from demanding `&mut`.
pub trait AiTagSink: AssetSource {
    /// Add `tags` to the asset's `tags_ai` namespace (never `tags_user`), as one signed
    /// metadata update. An empty `tags` is a no-op.
    fn add_ai_tags(&mut self, asset_id: &Uuid, tags: Vec<AiTag>) -> Result<(), Self::Error>;
}

/// A failure from the asset store behind the [`AssetSource`] / [`AiTagSink`] seam, type-erased.
///
/// For the `Workspace` implementation this wraps a
/// `LifecycleError`; `ml` cannot name that type without re-creating the cycle this seam broke, so
/// the concrete error is boxed and its message preserved.
#[derive(Debug, Error)]
#[error("asset store: {0}")]
pub struct StoreError(pub Box<dyn std::error::Error + Send + Sync>);

/// Box a store error into an [`OrchestratorError`].
fn store_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> OrchestratorError {
    OrchestratorError::Store(StoreError(Box::new(e)))
}

/// Failures from orchestrating inference.
#[derive(Debug, Error)]
pub enum OrchestratorError {
    /// A read or a tag write against the asset store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
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
pub fn embed_and_store<R: ModelRunner, S: AssetSource>(
    ws: &S,
    runner: &R,
    registry: &Registry,
    asset_id: &Uuid,
    task: TaskKind,
    partition: &str,
) -> Result<(), OrchestratorError> {
    require_canonical_runner(runner, registry, task)?;
    let (model_id, model_version) = runner.model(task).ok_or(RunnerError::Unsupported(task))?;
    let bytes = ws.read_plaintext(asset_id).map_err(store_err)?;
    let embedding = first_embedding(runner.embed_image(task, &[Frame::new(&bytes)])?)?;
    ws.vector_index().insert_embedding(
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
pub fn auto_tag<R: ModelRunner, S: AiTagSink>(
    ws: &mut S,
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

    let bytes = ws.read_plaintext(asset_id).map_err(store_err)?;
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
    ws.add_ai_tags(asset_id, ai_tags).map_err(store_err)?;
    Ok(assigned)
}

/// Natural-language search: embed `query` with the semantic-search embedder and return the `k`
/// nearest assets in `partition`'s **current canonical** version (stale/other-partition vectors are
/// excluded structurally).
pub fn semantic_search<R: ModelRunner, S: AssetSource>(
    ws: &S,
    runner: &R,
    registry: &Registry,
    query: &str,
    k: usize,
    partition: &str,
) -> Result<Vec<KnnHit>, OrchestratorError> {
    require_canonical_runner(runner, registry, TaskKind::SemanticSearch)?;
    let qv = first_embedding(runner.embed_text(&[query])?)?;
    Ok(ws
        .vector_index()
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
mod tests;
