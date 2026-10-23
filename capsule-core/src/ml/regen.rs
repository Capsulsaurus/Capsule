//! Version-bump regeneration — the background work-loop that rebuilds stale embeddings (SSoT:
//! [AI/ML — Embedding Provenance]).
//!
//! A model swap [increments](crate::ml::Registry::bump_version) a task's canonical `model_version`;
//! every embedding stored at the prior version is left **stale-flagged** and excluded from queries
//! ([`stale_embedding_assets`](crate::ml::VectorIndex::stale_embedding_assets) is the resulting
//! work-list). This module walks that list and re-embeds each asset at the new canonical version,
//! **replacing** the old vector per-asset — never a global truncate-and-rebuild — so
//! already-regenerated assets stay queryable throughout and the not-yet-done ones stay excluded
//! (the contract's "old entries are removed only after new ones persist").
//!
//! The loop is **resumable by construction**: it holds no cursor. The work-list is *derived* from
//! current staleness, so a run killed mid-loop simply re-derives the remaining stale assets on the
//! next call — a completed asset is no longer stale and never reappears; a not-yet-started one
//! still does. Passing a `budget` bounds each invocation (the background-task shape: call until
//! `remaining == 0`, yielding between chunks for the [thermal-throttle
//! pause](crate::ml) and micro-batch ceiling).
//!
//! Real per-platform inference is deferred behind the [`Embedder`] seam exactly as live MLS state
//! is deferred behind [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority): the
//! deterministic [`DeterministicEmbedder`] reference double drives the loop end-to-end with no
//! model weights, and a later slice supplies the on-device runner.
//!
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use thiserror::Error;
use uuid::Uuid;

use crate::ml::index::{EmbeddingInsert, VectorIndex, VectorIndexError};
use crate::ml::{AssetSource, Frame, ModelId, ModelRunner, ModelVersion, Registry, TaskKind};

/// The embedder seam the regeneration loop drives: "produce the canonical embedding for an asset
/// under a task". Abstracting it keeps the orchestration testable with a deterministic double and
/// independent of any real model runtime.
pub trait Embedder {
    /// The `(model_id, model_version)` this embedder produces for `task`, or `None` if it does not
    /// serve that task (e.g. a non-embedding task). The loop refuses an embedder whose declared
    /// pair is not the registry canonical — its outputs would otherwise be stored under a tuple
    /// they did not produce.
    fn model(&self, task: TaskKind) -> Option<(ModelId, ModelVersion)>;

    /// The `platform` partition discriminator for this embedder's outputs (incomparable across
    /// platforms; see the E2EE fallback in `ai.md`).
    fn platform(&self) -> &str;

    /// Produce the embedding vector for `asset_id` under `task`. Its length must equal the task's
    /// registry-declared dimension (the vector index re-checks this on insert).
    fn embed(&self, asset_id: &str, task: TaskKind) -> Result<Vec<f32>, EmbedError>;
}

/// A failure producing an embedding for an asset.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("embedding asset `{asset_id}` for {task:?} failed: {reason}")]
pub struct EmbedError {
    /// The asset that failed to embed.
    pub asset_id: String,
    /// The task being embedded.
    pub task: TaskKind,
    /// A human-readable reason (logging / diagnostics).
    pub reason: String,
}

/// Failures from the regeneration loop.
#[derive(Debug, Error)]
pub enum RegenError {
    /// The task has no canonical embedding model (unknown task, or a detection task with no stored
    /// vectors to regenerate).
    #[error("task {task:?} has no canonical embedding model to regenerate")]
    NotAnEmbeddingTask {
        /// The task.
        task: TaskKind,
    },
    /// The embedder does not produce the registry's canonical `(model_id, model_version)` for the
    /// task — regeneration is refused before any output is stored, so the index never gains a
    /// vector tagged with a tuple the embedder did not produce.
    #[error(
        "embedder model `{declared_id}` v`{declared_version}` is not canonical for task {task:?} \
         (canonical `{canonical_id}` v`{canonical_version}`)"
    )]
    NonCanonicalEmbedder {
        /// The task.
        task: TaskKind,
        /// The embedder's declared model id.
        declared_id: ModelId,
        /// The embedder's declared model version.
        declared_version: ModelVersion,
        /// The registry's canonical model id.
        canonical_id: ModelId,
        /// The registry's canonical model version.
        canonical_version: ModelVersion,
    },
    /// Producing an embedding failed.
    #[error(transparent)]
    Embed(#[from] EmbedError),
    /// Writing the fresh embedding to the vector index failed.
    #[error(transparent)]
    Vector(#[from] VectorIndexError),
}

/// The outcome of one regeneration invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegenReport {
    /// The task regenerated.
    pub task: TaskKind,
    /// How many stale embeddings were rebuilt in this invocation.
    pub regenerated: usize,
    /// How many stale embeddings remain in the partition after this invocation (`0` when drained).
    pub remaining: usize,
}

impl RegenReport {
    /// Whether the partition is fully regenerated (no stale entries remain).
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }
}

/// Regenerate stale embeddings for `task` in `embedder`'s platform partition, re-deriving the
/// work-list from [`stale_embedding_assets`](VectorIndex::stale_embedding_assets) on each call.
///
/// `budget` bounds the invocation: `Some(n)` rebuilds up to `n` assets (the background chunk;
/// call again until [`RegenReport::is_complete`]); `None` drains the whole partition. Assets are
/// processed in the work-list's sorted order, so a bounded run is deterministic and its remainder
/// is exactly the tail. Each asset's fresh vector is inserted with a per-asset replace before the
/// next is processed, so the run is safe to interrupt at any point.
///
/// Refuses a `NonCanonicalEmbedder` (an embedder not producing the current canonical tuple) before
/// touching the index.
pub fn regenerate_stale<I: VectorIndex, E: Embedder>(
    db: &I,
    registry: &Registry,
    embedder: &E,
    task: TaskKind,
    budget: Option<usize>,
) -> Result<RegenReport, RegenError> {
    let canon = registry
        .canonical_for(task)
        .filter(|r| r.embedding_spec().is_some())
        .ok_or(RegenError::NotAnEmbeddingTask { task })?;

    // The embedder must declare the current canonical tuple, or its outputs are refused wholesale.
    match embedder.model(task) {
        Some((id, version)) if id == canon.model_id && version == canon.canonical_version => {}
        other => {
            let (declared_id, declared_version) =
                other.unwrap_or_else(|| (ModelId::from(""), ModelVersion::from("")));
            return Err(RegenError::NonCanonicalEmbedder {
                task,
                declared_id,
                declared_version,
                canonical_id: canon.model_id.clone(),
                canonical_version: canon.canonical_version.clone(),
            });
        }
    }

    let platform = embedder.platform().to_string();
    let worklist = db.stale_embedding_assets(registry, task, &platform)?;
    let total = worklist.len();
    let take = budget.unwrap_or(total);
    tracing::info!(
        ?task,
        platform = %platform,
        model_id = %canon.model_id,
        model_version = %canon.canonical_version,
        stale = total,
        chunk = take,
        "ml regen: starting a regeneration chunk"
    );

    let mut regenerated = 0;
    for asset_id in worklist.iter().take(take) {
        tracing::trace!(?task, asset_id, "ml regen: re-embedding a stale asset");
        let vector = embedder.embed(asset_id, task)?;
        db.insert_embedding(
            registry,
            EmbeddingInsert {
                asset_id,
                task,
                model_id: &canon.model_id,
                model_version: &canon.canonical_version,
                platform: &platform,
                vector: &vector,
            },
        )?;
        regenerated += 1;
        tracing::debug!(
            ?task,
            asset_id,
            model_version = %canon.canonical_version,
            "ml regen: stale embedding replaced at the canonical version"
        );
    }

    let remaining = total - regenerated;
    tracing::info!(
        ?task,
        platform = %platform,
        regenerated,
        remaining,
        "ml regen: chunk complete"
    );
    Ok(RegenReport {
        task,
        regenerated,
        remaining,
    })
}

/// The production [`Embedder`]: regeneration **re-runs inference over the originals**.
///
/// This is the adapter that makes regeneration mean what [AI/ML — Embedding
/// Provenance](https://docs/design/ai/#embedding-provenance) says it means. The vector index is
/// derived state: a version bump invalidates it, and it is rebuilt by re-reading each asset's
/// original plaintext through the [`AssetSource`] seam and re-running the canonical model through
/// the [`ModelRunner`] seam — never by copying, re-labelling, or restoring the stale vector. It is
/// the same pair of seams [`embed_and_store`](crate::ml::embed_and_store) uses for first-time
/// indexing, so the regenerated vector is byte-identical to what a fresh import would produce.
///
/// `partition` is the `vec0` partition the outputs belong to — resolve it with
/// [`resolve_partition`](crate::ml::resolve_partition) rather than assuming
/// [`CANONICAL_PARTITION`](crate::ml::CANONICAL_PARTITION), so a device that failed the
/// known-answer check regenerates within its own partition instead of polluting the shared one.
#[derive(Debug, Clone, Copy)]
pub struct RunnerEmbedder<'a, R, S> {
    /// Where the originals come from (and whose catalog holds the index).
    source: &'a S,
    /// The inference seam the canonical model runs behind.
    runner: &'a R,
    /// The `vec0` partition these outputs belong to.
    partition: &'a str,
}

impl<'a, R: ModelRunner, S: AssetSource> RunnerEmbedder<'a, R, S> {
    /// An embedder that reads `source`'s originals and re-runs `runner` over them, writing into
    /// `partition`.
    pub fn new(source: &'a S, runner: &'a R, partition: &'a str) -> Self {
        Self {
            source,
            runner,
            partition,
        }
    }

    /// The catalog holding the vector index these embeddings are regenerated into — pass it to
    /// [`regenerate_stale`] so the work-list and the writes address one index.
    pub fn index(&self) -> &'a S::Index {
        self.source.vector_index()
    }
}

impl<R: ModelRunner, S: AssetSource> Embedder for RunnerEmbedder<'_, R, S> {
    fn model(&self, task: TaskKind) -> Option<(ModelId, ModelVersion)> {
        self.runner.model(task)
    }

    fn platform(&self) -> &str {
        self.partition
    }

    fn embed(&self, asset_id: &str, task: TaskKind) -> Result<Vec<f32>, EmbedError> {
        let fail = |reason: String| EmbedError {
            asset_id: asset_id.to_string(),
            task,
            reason,
        };
        // Index rows key assets by their string form; the store keys them by UUID.
        let uuid =
            Uuid::parse_str(asset_id).map_err(|e| fail(format!("asset id is not a UUID: {e}")))?;
        // The originals are the source of truth the derived index is rebuilt from.
        let bytes = self
            .source
            .read_plaintext(&uuid)
            .map_err(|e| fail(format!("reading the original failed: {e}")))?;
        self.runner
            .embed_image(task, &[Frame::new(&bytes)])
            .map_err(|e| fail(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| fail("runner returned no embedding".to_string()))
    }
}

/// A deterministic reference embedder — **no real inference**, no model weights. It maps
/// `(asset_id, canonical_version)` to a stable L2-normalized vector, so:
///
/// - the same asset re-embeds identically within a version (idempotent inserts);
/// - a version bump yields a *different* vector — a genuine re-embed, not a copy of the old one;
/// - a query is reproduced by [embedding the same key](Self::embed_key), giving an exact match.
///
/// It stands in for the on-device model runner the way
/// [`ReferenceAuthority`](crate::crypto::authority::ReferenceAuthority) stands in for live MLS
/// state: enough to exercise the regeneration orchestration end-to-end and deterministically.
#[derive(Debug, Clone)]
pub struct DeterministicEmbedder {
    registry: Registry,
    platform: String,
}

impl DeterministicEmbedder {
    /// A reference embedder for `platform` producing the current canonical inventory.
    pub fn new(platform: impl Into<String>) -> Self {
        Self {
            registry: Registry::canonical(),
            platform: platform.into(),
        }
    }

    /// A reference embedder pinned to a specific `registry` — use a version-bumped registry to
    /// model the post-swap model that produces the new canonical version.
    pub fn with_registry(platform: impl Into<String>, registry: Registry) -> Self {
        Self {
            registry,
            platform: platform.into(),
        }
    }

    /// Embed an arbitrary `key` (an asset id, or a query concept) at `task`'s canonical version —
    /// the query side of the reference vector space. Returns an empty vector for a non-embedding
    /// task.
    pub fn embed_key(&self, key: &str, task: TaskKind) -> Vec<f32> {
        match self.registry.canonical_for(task).and_then(|r| {
            r.embedding_spec()
                .map(|(dim, _)| (dim.get(), r.canonical_version.clone()))
        }) {
            Some((dim, version)) => seeded_unit_vector(key, version.as_str(), dim),
            None => Vec::new(),
        }
    }
}

impl Embedder for DeterministicEmbedder {
    fn model(&self, task: TaskKind) -> Option<(ModelId, ModelVersion)> {
        let row = self.registry.canonical_for(task)?;
        // Only embedding tasks produce stored vectors.
        row.embedding_spec()?;
        Some((row.model_id.clone(), row.canonical_version.clone()))
    }

    fn platform(&self) -> &str {
        &self.platform
    }

    fn embed(&self, asset_id: &str, task: TaskKind) -> Result<Vec<f32>, EmbedError> {
        let row = self
            .registry
            .canonical_for(task)
            .ok_or_else(|| EmbedError {
                asset_id: asset_id.to_string(),
                task,
                reason: "no canonical model for task".into(),
            })?;
        let (dim, _) = row.embedding_spec().ok_or_else(|| EmbedError {
            asset_id: asset_id.to_string(),
            task,
            reason: "task does not produce stored embeddings".into(),
        })?;
        Ok(seeded_unit_vector(
            asset_id,
            row.canonical_version.as_str(),
            dim.get(),
        ))
    }
}

/// FNV-1a 64-bit hash — a small, dependency-free, deterministic mixer for seeding.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// SplitMix64 — a deterministic PRNG step for filling a vector from a seed.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A deterministic L2-normalized vector of length `dim`, seeded by `(key, version)`. Distinct keys
/// give distinct directions; the same key at a different version gives a distinct vector (so a
/// version bump is a genuine re-embed). Embeddings are normalized, so cosine ranks as the inner
/// product — matching the vector index's metric.
fn seeded_unit_vector(key: &str, version: &str, dim: usize) -> Vec<f32> {
    let mut state = fnv1a64(key.as_bytes()) ^ fnv1a64(version.as_bytes()).rotate_left(32);
    let mut v = Vec::with_capacity(dim);
    for _ in 0..dim {
        // Top 53 bits → a double in [0, 1); map to [-1, 1).
        let unit = (splitmix64(&mut state) >> 11) as f64 / (1u64 << 53) as f64;
        v.push((unit * 2.0 - 1.0) as f32);
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    } else if dim > 0 {
        v[0] = 1.0;
    }
    v
}
