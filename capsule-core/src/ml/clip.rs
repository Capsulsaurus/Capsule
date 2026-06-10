//! A real CLIP [`ModelRunner`] — pure-Rust inference via candle (SSoT: [AI/ML Integrations]).
//!
//! Compiled only under the **default-off `inference` feature**, so the standard build/test/CI
//! never pulls the ML stack and **never runs inference**. Weights are fetched from Hugging Face at
//! runtime into the local HF cache (outside this repository) — **no weights are ever committed**.
//!
//! This is a *base model* ([`openai/clip-vit-base-patch32`]) wired up now so the local-validation
//! infrastructure is real and exercisable; the committed inventory choice (MobileCLIP-B) and other
//! models are refined later. Because the runner declares its own `model_id` (`clip-vit-base-patch32`),
//! a deployment using it sets the registry's canonical model to match via
//! [`Registry::set_canonical_model`](crate::ml::Registry::set_canonical_model) — keeping the
//! embedding-provenance tuple honest.
//!
//! Run the local validation (never in CI):
//! ```text
//! cargo test -p capsule-core --features inference -- --ignored
//! ```
//!
//! [AI/ML Integrations]: https://docs/design/ai/
//! [`openai/clip-vit-base-patch32`]: https://huggingface.co/openai/clip-vit-base-patch32

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::clip::{ClipConfig, ClipModel};
use thiserror::Error;
use tokenizers::Tokenizer;

use crate::ml::runner::{Detection, Embedding, Frame, ModelRunner, RunnerError};
use crate::ml::{ModelId, ModelVersion, TaskKind};

/// The Hugging Face repo + revision the weights are fetched from.
const MODEL_REPO: &str = "openai/clip-vit-base-patch32";
const MODEL_REVISION: &str = "refs/pr/15";
/// The runner's declared model id / version (the embedding-provenance tuple).
pub const CLIP_MODEL_ID: &str = "clip-vit-base-patch32";
pub const CLIP_MODEL_VERSION: &str = "1";

/// Failure loading the CLIP model or its tokenizer.
#[derive(Debug, Error)]
pub enum ClipError {
    /// Fetching weights / tokenizer from the Hugging Face hub failed.
    #[error("hugging face hub: {0}")]
    Hub(String),
    /// Loading the model with candle failed.
    #[error("candle: {0}")]
    Candle(String),
    /// Loading the tokenizer failed.
    #[error("tokenizer: {0}")]
    Tokenizer(String),
}

/// A CLIP image+text embedder backed by candle. Embeds images and natural-language text into one
/// 512-d space; identical to the [`ModelRunner`] contract the orchestrator drives.
pub struct ClipRunner {
    platform: String,
    model: ClipModel,
    config: ClipConfig,
    tokenizer: Tokenizer,
    device: Device,
}

impl ClipRunner {
    /// Load the base CLIP model, fetching `model.safetensors` + `tokenizer.json` from Hugging Face
    /// into the local cache on first use. `platform` tags the embeddings' partition.
    pub fn load(platform: &str) -> Result<Self, ClipError> {
        let device = Device::Cpu;
        let api = hf_hub::api::sync::Api::new().map_err(|e| ClipError::Hub(e.to_string()))?;
        let repo = api.repo(hf_hub::Repo::with_revision(
            MODEL_REPO.to_string(),
            hf_hub::RepoType::Model,
            MODEL_REVISION.to_string(),
        ));
        let model_file = repo
            .get("model.safetensors")
            .map_err(|e| ClipError::Hub(e.to_string()))?;
        let tokenizer_file = repo
            .get("tokenizer.json")
            .map_err(|e| ClipError::Hub(e.to_string()))?;

        let config = ClipConfig::vit_base_patch32();
        // SAFETY: memory-mapping a trusted, just-downloaded safetensors file.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_file], DType::F32, &device)
                .map_err(|e| ClipError::Candle(e.to_string()))?
        };
        let model = ClipModel::new(vb, &config).map_err(|e| ClipError::Candle(e.to_string()))?;
        let tokenizer = Tokenizer::from_file(tokenizer_file)
            .map_err(|e| ClipError::Tokenizer(e.to_string()))?;

        Ok(Self {
            platform: platform.to_string(),
            model,
            config,
            tokenizer,
            device,
        })
    }

    /// Decode + preprocess one image's bytes into a `(3, image_size, image_size)` F32 tensor
    /// normalized to `[-1, 1]` — the CLIP image-encoder input.
    fn preprocess_image(&self, bytes: &[u8]) -> Result<Tensor, RunnerError> {
        let size = self.config.image_size;
        let img = image::load_from_memory(bytes)
            .map_err(infer)?
            .resize_exact(
                size as u32,
                size as u32,
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8();
        let data = img.into_raw();
        Tensor::from_vec(data, (size, size, 3), &self.device)
            .map_err(infer)?
            .permute((2, 0, 1))
            .map_err(infer)?
            .to_dtype(DType::F32)
            .map_err(infer)?
            .affine(2.0 / 255.0, -1.0)
            .map_err(infer)
    }
}

impl ModelRunner for ClipRunner {
    fn platform(&self) -> &str {
        &self.platform
    }

    fn model(&self, task: TaskKind) -> Option<(ModelId, ModelVersion)> {
        match task {
            TaskKind::SemanticSearch => Some((
                ModelId::from(CLIP_MODEL_ID),
                ModelVersion::from(CLIP_MODEL_VERSION),
            )),
            _ => None,
        }
    }

    fn embed_image(
        &self,
        task: TaskKind,
        frames: &[Frame<'_>],
    ) -> Result<Vec<Embedding>, RunnerError> {
        if task != TaskKind::SemanticSearch {
            return Err(RunnerError::Unsupported(task));
        }
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let tensors: Vec<Tensor> = frames
            .iter()
            .map(|f| self.preprocess_image(f.bytes))
            .collect::<Result<_, _>>()?;
        let batch = Tensor::stack(&tensors, 0).map_err(infer)?;
        let feats = self.model.get_image_features(&batch).map_err(infer)?;
        rows_l2_normalized(&feats)
    }

    fn embed_text(&self, texts: &[&str]) -> Result<Vec<Embedding>, RunnerError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let pad_id = self.tokenizer.token_to_id("<|endoftext|>").unwrap_or(0);
        let mut rows: Vec<Vec<u32>> = Vec::with_capacity(texts.len());
        let mut max_len = 0usize;
        for t in texts {
            let enc = self.tokenizer.encode(*t, true).map_err(infer)?;
            let ids = enc.get_ids().to_vec();
            max_len = max_len.max(ids.len());
            rows.push(ids);
        }
        for ids in &mut rows {
            ids.resize(max_len, pad_id);
        }
        let n = rows.len();
        let flat: Vec<u32> = rows.into_iter().flatten().collect();
        let input_ids = Tensor::from_vec(flat, (n, max_len), &self.device).map_err(infer)?;
        let feats = self.model.get_text_features(&input_ids).map_err(infer)?;
        rows_l2_normalized(&feats)
    }

    fn detect(
        &self,
        task: TaskKind,
        _frames: &[Frame<'_>],
    ) -> Result<Vec<Vec<Detection>>, RunnerError> {
        // CLIP is a semantic embedder, not a detector.
        Err(RunnerError::Unsupported(task))
    }
}

fn infer<E: std::fmt::Display>(e: E) -> RunnerError {
    RunnerError::Inference(e.to_string())
}

/// Convert a `(rows, dim)` F32 tensor into L2-normalized vectors.
fn rows_l2_normalized(feats: &Tensor) -> Result<Vec<Embedding>, RunnerError> {
    let rows: Vec<Embedding> = feats.to_vec2::<f32>().map_err(infer)?;
    Ok(rows
        .into_iter()
        .map(|mut row| {
            let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut row {
                    *x /= norm;
                }
            }
            row
        })
        .collect())
}

#[cfg(test)]
mod tests {
    //! Local-validation harness. Every test here is `#[ignore]` — it fetches real weights from
    //! Hugging Face and runs inference, which is far too expensive for CI. Run locally with:
    //! `cargo test -p capsule-core --features inference -- --ignored`.

    use super::*;
    use crate::ml::Registry;
    use crate::ml::reid::cosine_sim;

    const PLATFORM: &str = "candle-clip-cpu";

    /// A solid-color PNG of CLIP's input size, encoded in memory (no committed fixture).
    fn solid_png(rgb: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(224, 224, image::Rgb(rgb));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    #[ignore = "fetches real weights + runs inference; run locally with --features inference -- --ignored"]
    fn embeddings_are_deterministic_normalized_512d() {
        let runner = ClipRunner::load(PLATFORM).expect("load clip");
        let png = solid_png([200, 30, 30]);
        let a = runner
            .embed_image(TaskKind::SemanticSearch, &[Frame::new(&png)])
            .unwrap();
        let b = runner
            .embed_image(TaskKind::SemanticSearch, &[Frame::new(&png)])
            .unwrap();
        assert_eq!(a, b, "same image ⇒ same embedding (deterministic)");
        assert_eq!(a[0].len(), 512);
        let norm = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "L2-normalized");

        let txt = runner.embed_text(&["a photo of a cat"]).unwrap();
        assert_eq!(txt[0].len(), 512);
    }

    #[test]
    #[ignore = "fetches real weights + runs inference; run locally with --features inference -- --ignored"]
    fn text_semantics_order_correctly() {
        let runner = ClipRunner::load(PLATFORM).expect("load clip");
        let v = runner
            .embed_text(&[
                "a photo of a cat",
                "a photo of a kitten",
                "a photo of a truck",
            ])
            .unwrap();
        let cat_kitten = cosine_sim(&v[0], &v[1]);
        let cat_truck = cosine_sim(&v[0], &v[2]);
        assert!(
            cat_kitten > cat_truck,
            "cat~kitten ({cat_kitten:.3}) should exceed cat~truck ({cat_truck:.3})"
        );
    }

    #[test]
    #[ignore = "fetches real weights + runs inference; run locally with --features inference -- --ignored"]
    fn image_text_alignment_through_the_orchestrator() {
        use crate::crypto::primitives::Argon2Params;
        use crate::lifecycle::Workspace;
        use crate::ml::{embed_and_store, semantic_search};
        use tempfile::TempDir;

        // A deployment registry whose canonical SemanticSearch model is the one this runner runs.
        let mut registry = Registry::canonical();
        registry.set_canonical_model(
            TaskKind::SemanticSearch,
            ModelId::from(CLIP_MODEL_ID),
            ModelVersion::from(CLIP_MODEL_VERSION),
        );
        let runner = ClipRunner::load(PLATFORM).expect("load clip");

        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let red_path = src.path().join("red.png");
        let blue_path = src.path().join("blue.png");
        std::fs::write(&red_path, solid_png([220, 20, 20])).unwrap();
        std::fs::write(&blue_path, solid_png([20, 20, 220])).unwrap();

        let mut ws = Workspace::create_with_params(
            lib.path(),
            b"p",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        let album = ws.create_album("A");
        let red = ws.import_asset(album, &red_path).unwrap();
        let blue = ws.import_asset(album, &blue_path).unwrap();

        embed_and_store(&mut ws, &runner, &registry, &red, TaskKind::SemanticSearch).unwrap();
        embed_and_store(&mut ws, &runner, &registry, &blue, TaskKind::SemanticSearch).unwrap();

        // A "red" query should rank the red image ahead of the blue one (real image↔text alignment).
        let hits = semantic_search(&ws, &runner, &registry, "a solid red image", 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].asset_id,
            red.to_string(),
            "the red image should be the nearest hit for a red query"
        );
        let _ = blue;
    }
}
