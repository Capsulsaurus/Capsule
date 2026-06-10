//! The inference seam — a [`ModelRunner`] trait the orchestrator consumes, plus a deterministic
//! [`FixtureRunner`] for tests (SSoT: [AI/ML Integrations]).
//!
//! Real per-platform runners (ONNX Runtime / CoreML / NNAPI, behind a default-off, weight-fetching
//! feature) implement this trait. Until then `FixtureRunner` stands in — exactly as
//! [`ReferenceAuthority`](crate::crypto::authority::ReferenceAuthority) stands in for live MLS
//! behind [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority). The fixture maps an input's
//! bytes to a reproducible L2-normalized vector by expanding SHA-256, so identical content embeds
//! identically and semantic matches are testable without any model weights.
//!
//! [AI/ML Integrations]: https://docs/design/ai/

use thiserror::Error;

use crate::crypto::hash;
use crate::ml::{ModelId, ModelVersion, Registry, TaskKind};

/// An embedding vector (L2-normalized by convention, so cosine ranks like the inner product).
pub type Embedding = Vec<f32>;

/// One image (or video keyframe) handed to a runner. The offline core passes the asset's bytes; a
/// real runner decodes + runs the model, the fixture hashes them.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// The image bytes.
    pub bytes: &'a [u8],
}

impl<'a> Frame<'a> {
    /// A frame over `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

/// A normalized bounding box (coordinates in `[0, 1]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// A detection from a detection task (object box, face box + landmarks downstream).
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// Where.
    pub bbox: BBox,
    /// Optional class label.
    pub label: Option<String>,
    /// Confidence in `[0, 1]`.
    pub score: f32,
}

/// Errors a runner can return.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RunnerError {
    /// The runner cannot serve this task (e.g. an embedding call on a detection task).
    #[error("runner does not support task {0:?}")]
    Unsupported(TaskKind),
    /// Inference failed.
    #[error("inference failed: {0}")]
    Inference(String),
}

/// The inference seam every consumer routes through. Implemented by real per-platform runners and
/// by [`FixtureRunner`] for tests.
pub trait ModelRunner {
    /// The platform partition tag for this runner's outputs (incomparable across platforms).
    fn platform(&self) -> &str;

    /// The `(model_id, model_version)` this runner produces for `task`, if it serves it. Must equal
    /// the registry's canonical row for the output to be accepted by the index.
    fn model(&self, task: TaskKind) -> Option<(ModelId, ModelVersion)>;

    /// Embed images for an embedding task (`SemanticSearch`, `FaceRecognition`). One vector per
    /// frame, in order.
    fn embed_image(
        &self,
        task: TaskKind,
        frames: &[Frame<'_>],
    ) -> Result<Vec<Embedding>, RunnerError>;

    /// Embed text into the semantic-search space — for natural-language queries and the zero-shot
    /// tagging that reuses the semantic embedder.
    fn embed_text(&self, texts: &[&str]) -> Result<Vec<Embedding>, RunnerError>;

    /// Detect objects / faces for a detection task (`ObjectDetection`, `FaceDetection`). One list
    /// per frame, in order.
    fn detect(
        &self,
        task: TaskKind,
        frames: &[Frame<'_>],
    ) -> Result<Vec<Vec<Detection>>, RunnerError>;
}

/// Expand SHA-256 of `bytes` into a deterministic, L2-normalized `dim`-vector. Identical bytes
/// produce an identical vector; distinct bytes produce near-orthogonal vectors.
fn deterministic_embedding(bytes: &[u8], dim: usize) -> Embedding {
    let mut out: Embedding = Vec::with_capacity(dim);
    let mut counter: u32 = 0;
    while out.len() < dim {
        let mut input = bytes.to_vec();
        input.extend_from_slice(&counter.to_le_bytes());
        let digest = hash::hash_bytes(&input);
        for pair in digest.0.chunks_exact(2) {
            if out.len() == dim {
                break;
            }
            // Map two bytes to roughly [-1, 1).
            let v = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0;
            out.push(v);
        }
        counter += 1;
    }
    l2_normalize(&mut out);
    out
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// A deterministic, weightless runner for tests. Declares the canonical model versions from a
/// [`Registry`] (so its outputs satisfy the embedding-provenance invariant) and embeds by hashing
/// input bytes — content equality ⇒ vector equality, which makes semantic matches reproducible.
#[derive(Debug, Clone)]
pub struct FixtureRunner {
    platform: String,
    registry: Registry,
}

impl FixtureRunner {
    /// A fixture on `platform` declaring the canonical inventory's model versions.
    pub fn new(platform: &str) -> Self {
        Self {
            platform: platform.to_string(),
            registry: Registry::canonical(),
        }
    }

    /// A fixture whose declared model versions come from `registry` — for simulating a model swap
    /// (a runner that produces the *new* canonical version after a bump).
    pub fn with_registry(platform: &str, registry: Registry) -> Self {
        Self {
            platform: platform.to_string(),
            registry,
        }
    }

    fn dim(&self, task: TaskKind) -> Result<usize, RunnerError> {
        self.registry
            .dim_for(task)
            .map(|d| d.get())
            .ok_or(RunnerError::Unsupported(task))
    }
}

impl ModelRunner for FixtureRunner {
    fn platform(&self) -> &str {
        &self.platform
    }

    fn model(&self, task: TaskKind) -> Option<(ModelId, ModelVersion)> {
        self.registry
            .canonical_for(task)
            .map(|r| (r.model_id.clone(), r.canonical_version.clone()))
    }

    fn embed_image(
        &self,
        task: TaskKind,
        frames: &[Frame<'_>],
    ) -> Result<Vec<Embedding>, RunnerError> {
        let dim = self.dim(task)?;
        Ok(frames
            .iter()
            .map(|f| deterministic_embedding(f.bytes, dim))
            .collect())
    }

    fn embed_text(&self, texts: &[&str]) -> Result<Vec<Embedding>, RunnerError> {
        // Text shares the semantic-search space.
        let dim = self.dim(TaskKind::SemanticSearch)?;
        Ok(texts
            .iter()
            .map(|t| deterministic_embedding(t.as_bytes(), dim))
            .collect())
    }

    fn detect(
        &self,
        task: TaskKind,
        frames: &[Frame<'_>],
    ) -> Result<Vec<Vec<Detection>>, RunnerError> {
        match task {
            TaskKind::ObjectDetection | TaskKind::FaceDetection => {
                // The fixture has no real detector; it reports no detections deterministically.
                Ok(frames.iter().map(|_| Vec::new()).collect())
            }
            other => Err(RunnerError::Unsupported(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fixture_declares_canonical_models() {
        let r = FixtureRunner::new("cpu-reference");
        assert_eq!(
            r.model(TaskKind::SemanticSearch),
            Some((ModelId::from("mobileclip-b"), ModelVersion::from("1")))
        );
        assert_eq!(
            r.model(TaskKind::FaceDetection),
            Some((ModelId::from("scrfd"), ModelVersion::from("1")))
        );
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
}
