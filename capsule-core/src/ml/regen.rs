//! Version-bump regeneration — the background work-loop that rebuilds stale embeddings (SSoT:
//! [AI/ML — Embedding Provenance]).
//!
//! A model swap [increments](crate::ml::Registry::bump_version) a task's canonical `model_version`;
//! every embedding stored at the prior version is left **stale-flagged** and excluded from queries
//! ([`stale_embedding_assets`](crate::db::DatabaseDriver::stale_embedding_assets) is the resulting
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

use crate::db::{DatabaseDriver, EmbeddingInsert, VectorIndexError};
use crate::ml::{ModelId, ModelVersion, Registry, TaskKind};

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
/// work-list from [`stale_embedding_assets`](DatabaseDriver::stale_embedding_assets) on each call.
///
/// `budget` bounds the invocation: `Some(n)` rebuilds up to `n` assets (the background chunk;
/// call again until [`RegenReport::is_complete`]); `None` drains the whole partition. Assets are
/// processed in the work-list's sorted order, so a bounded run is deterministic and its remainder
/// is exactly the tail. Each asset's fresh vector is inserted with a per-asset replace before the
/// next is processed, so the run is safe to interrupt at any point.
///
/// Refuses a `NonCanonicalEmbedder` (an embedder not producing the current canonical tuple) before
/// touching the index.
pub fn regenerate_stale<E: Embedder>(
    db: &DatabaseDriver,
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
    let worklist = db
        .stale_embedding_assets(registry, task, &platform)
        .map_err(|e| RegenError::Vector(e.into()))?;
    let total = worklist.len();
    let take = budget.unwrap_or(total);

    let mut regenerated = 0;
    for asset_id in worklist.iter().take(take) {
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
    }

    Ok(RegenReport {
        task,
        regenerated,
        remaining: total - regenerated,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Bounded E2E — Module Map E2E case #10 ("Model regen after version bump"): bump the canonical
    /// model version → stale embeddings excluded from queries → background regen produces fresh
    /// embeddings per-asset → queries return correct results post-regen. Covers `capsule-core::ml`
    /// (registry swap + regen loop) × `capsule-core::db` vector index, with the deterministic
    /// embedder standing in for the on-device runner.
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
}
