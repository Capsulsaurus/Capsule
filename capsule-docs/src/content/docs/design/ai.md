---
title: AI/ML Integrations
description: AI feature architecture, the canonical model inventory, embedding provenance, and AI/user metadata separation
---

Capsule runs a hierarchy of ML models, all **client-side** (the server never holds plaintext). The stable contract is the *structure*: three functional categories, the AI/user namespace separation in [AI Output Containment](#ai-output-containment), the canonical model inventory in [Models and Algorithms](#models-and-algorithms), and the [embedding-provenance](#embedding-provenance) invariant. The specific feature list and per-model choices are current defaults that will evolve with field testing.

The three categories:

- **[Semantic Indexing](#semantic-indexing):** a *global* embedding per asset for natural-language and similarity search.
- **[Dense Tagging](#dense-tagging):** *local* embeddings for objects, faces, and scene elements for granular search and auto-albums.
- **[Quality Assessment](#quality-assessment):** per-asset quality scores for filtering and sorting.

Inference orchestration lives in `capsule-core::ml`; per-platform model runners (CoreML, NNAPI, ONNX Runtime) live in `capsule-sdk`; the local vector index lives in `capsule-core::db` (SQLite + `sqlite-vec`).

## AI Output Containment

AI inference can be wrong, biased, or hallucinatory. A core rule prevents it from corrupting user intent: **AI outputs land in a separate namespace from user-authored metadata, structurally, not by policy.** The shape of the separation — `tags_ai` vs `tags_user` OR-sets, plus distinct sidecar fields for AI-derived facets — is owned by [Metadata — Tag Provenance and Namespacing](/design/metadata/#tag-provenance-and-namespacing); the consequences for AI features:

- An AI tag can never overwrite a user tag — they live in different fields, so the question does not arise.
- As each AI facet ships, it lands in its own AI-namespaced sidecar field the user does not directly edit (illustratively `ai_face_labels`, `ai_scene`, `ai_quality_score`) — reserved alongside `tags_ai` in the [sidecar schema](/design/metadata/#sidecar-schema-v1) and added when the feature is committed, never overlapping a user field. User corrections write to *user* fields; AI re-runs leave them alone.
- Every AI output carries `(model_id, model_version)` ([Embedding Provenance](#embedding-provenance)). When the canonical model for a slot changes, old outputs are flagged stale and excluded from queries until regenerated.
- Promoting an AI tag to a user tag is an explicit, signed [lifecycle operation](/design/authorization/#the-closed-action-set) — never automatic.

A hallucinating model can pollute its own namespace, never user intent. This is the structural defense against the "AI mistake silently overwrites user data" damage class — see [Threat Model — Forbidden Client Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors).

## Semantic Indexing

Semantic search converts an image and a text query into vectors and measures their distance. Because embeddings are generated client-side, every device must run the same canonical model along a deterministic path so vectors are comparable — the constraint and its platform-partition fallback are specified in [Embedding Provenance](#embedding-provenance).

### Image Categorization & Tagging

The semantic embeddings are reused for zero-shot classification to generate tags, enabling faceted search and auto-album generation without a separate classifier.

## Dense Tagging

Face Detection & Matching (clustering) runs the **Face Detection** and **Face Recognition** rows of the [model inventory](#models-and-algorithms) — SOTA-small models that run near-instantly on mobile.

## Quality Assessment

Deferred to post-v1. The category and its sidecar fields are reserved in the [containment model](#ai-output-containment) so it can land later without a schema change; the Quality candidate models in the [inventory](#models-and-algorithms) are not part of the v1 pipeline.

## Model Batching

On-device inference is memory- and power-bound, so execution mode is chosen per device:

- **Horizontal (model-by-model)** vs. **vertical (all models per asset)**: horizontal minimizes resident models at the cost of re-reading assets; vertical minimizes I/O but risks OOM on mobile. The mode is picked from available RAM at task start.
- **Micro-batching** (1/4/8 images) keeps the NPU cache hot; **INT8/FP16 quantization** halves memory bandwidth; **thermal throttling** pauses the pipeline past a temperature threshold (e.g. 40 °C) so the OS does not kill the app.

## Database Indexing and View Generation

Embeddings share a common vector space and are stored locally in **SQLite + `sqlite-vec`**. The vector index is **derived state, not a source of truth** ([recovery-first](/design/principles/)): if lost or corrupted it is rebuilt by re-running inference over the originals — the same path a model-version bump takes ([Embedding Provenance](#embedding-provenance)).

## Embedding Provenance

Every embedding Capsule stores — in the local SQLite vector index, in an encrypted backup, or inside a [`DerivativeManifest`](/design/cryptography/provenance/#derivative-provenance) for an embedding-class derivative — carries the tuple `(model_id, model_version)` identifying which [inventory](#models-and-algorithms) row produced it. Vector spaces differ across pairs, so embeddings are not comparable across `(model_id, model_version)`. Every `model_id` is declared in exactly one inventory row ([SSoT](/design/principles/#single-source-of-truth)); a swap is a one-row edit that propagates by `model_id` to every consumer. The invariant:

- The vector index **refuses inserts** whose `model_id` is not the current canonical row for its task. A buggy or new client uploading embeddings from an unrecognized model is rejected at the insert API, never silently mixed in.
- A model swap increments `model_version` for that task. Old embeddings are **flagged stale** and excluded from queries until regenerated from the originals. Cross-version comparison is forbidden — see [Threat Model — Client-Side Validation Invariants](/design/threat-model/validation/#client-side-validation-invariants).
- Regeneration is a background task that walks the library producing fresh embeddings at the new version; old entries are removed only after new ones persist (per-asset replace, not a global truncate-and-rebuild).

**E2EE constraint and its fallback.** Comparable embeddings need byte-identical inference output across heterogeneous NPUs/CPUs, and floating-point inference is not inherently reproducible across execution providers. So every device pins a **deterministic execution path** for the canonical model (fixed operator set, nondeterministic kernels disabled, quantized weights) and passes a byte-identical known-answer check; the model size floor is the lowest-end device Capsule supports, not the desktop. If a device cannot reach bit-exactness in field testing, the **fallback is explicit, never silent**: its embeddings are tagged with a `platform` discriminator and are **not merged** into another platform's index — they are regenerated locally and compared only within their own partition via tolerance-based ANN. The worst case is duplicated per-platform regeneration, never wrong search results. This defeats the "silent invalidation of the vector index" damage class ([Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) row #14).

## Models and Algorithms

One row per task. Where the size/accuracy trade allows, a single backbone is **reused across tasks** rather than loading one model per task — e.g. the canonical Semantic Search embedder also feeds zero-shot tagging and semantic-duplicate detection, and YOLOv10 serves both person and object detection. Reuse is the default whenever it does not measurably hurt quality; it is the main lever bounding peak VRAM on mobile.

### v1-Committed Slots

These four are the launch pipeline:

| Task                 | Model(s)                                                          | Function                                                                                                                |
| -------------------- | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Semantic Search**  | MobileCLIP-B (ONNX, INT8); quantized SigLIP-tiny fallback[^semantic-alt] | Global image embedding for natural-language + similarity search; sized for the lowest-end device (see the E2EE constraint above). |
| **Object Detection** | YOLOv10[^objdet-alt]                                             | Object/background detection feeding dense tagging; the backbone is reused for person detection.                        |
| **Face Detection**   | SCRFD                                                            | Efficient face bounding-box + landmark detection.                                                                      |
| **Face Recognition** | InsightFace (AdaFace)                                            | Face embeddings; AdaFace handles low-quality/dark images well.                                                         |

### Candidate Tasks (post-v1)

Planned tasks whose model choice is still subject to 2026 field testing. Each commits to a full inventory row (with datasets and the embedding-provenance tuple) when it ships:

- **Natural language & VLM** — Dense Tagging & OCR (Florence-2); Image Chat (Qwen2.5-VL or LLaVA-1.6); Captioning (BLIP-2).
- **People** — Person Detection (YOLOv10); Person Re-ID (OSNet or TransReID); Expression Analysis (EmotioNet); Quality Scoring (LIQE / TOPIQ).
- **Scene** — Scene Classification (ViT-L, ConvNeXt-L); Landmark Detection (DINOv2 + GeM); Bird/plant (BioCLIP); General animal (YOLOv8 fine-tuned); Screenshot detection (custom CNN).
- **Text & audio** — OCR (TrOCR); Voice Transcription (Distil-Whisper-large[^asr-alt]).
- **Quality** — Aesthetic (NIMA); Blur (Laplacian variance + CNN); Exposure (CNN regressor); Noise (CNN regressor).
- **Similarity** — Near-duplicate / burst (pHash/dHash + CNN); Semantic near-duplicate (canonical Semantic Search embeddings + ANN); Best-shot selection (quality models combined).
- **Video** — Shot/scene boundary (TransNet v2, PySceneDetect); Highlight extraction (temporal attention + quality score); Action recognition (VideoMAE, TimeSformer).
- **Categorization** — NSFW (OpenCLIP or custom CNN); Violence / graphic content (ViT classifier), e.g. for shared-album flagging.

[^semantic-alt]: Considered and rejected: SigLIP-so400m (~400M params, impractical on the lowest-end mobile we support — the E2EE constraint forces every device to run the same model), full CLIP ViT-L/14 (similar size class), OpenCLIP ViT-G (much larger). MobileCLIP-B is the size sweet spot; quantized SigLIP-tiny stays as a fallback if MobileCLIP semantic quality is insufficient in field tests.
[^objdet-alt]: Considered and rejected for the *committed* slot: Grounding DINO (open-vocabulary; heavier; revisit if dense-tagging breadth becomes the bottleneck), RT-DETR (transformer-based; comparable accuracy, slower on mobile). YOLOv10 is the committed choice; alternatives may run as additional specialized passes later.
[^asr-alt]: Considered and rejected: Whisper-large-v3 (best accuracy but too slow on mobile for opportunistic background transcription), Whisper-medium (similar speed to Distil-Whisper-large but worse accuracy), faster-whisper CT2 ports (a runtime optimization layer; can be applied on top of Distil-Whisper).

### Key Algorithmic Implementations

#### Video-as-Sparse-Photos

Processing every frame through heavy models is prohibitive, so video is treated as a sparse collection of keyframes:

1. **Cut Detection:** PySceneDetect (content-aware) chunks the video into visually distinct scenes.
2. **Temporal Sampling:** extract frames at the 10%, 50%, and 90% timestamps of each scene.
3. **Blur Rejection:** compute the variance of the Laplacian $V = \text{var}(\nabla^2 I)$; frames below a threshold are discarded as too blurry.
4. **Audio Processing:** run the canonical ASR model (the **Voice Transcription** row) concurrently for a timestamped transcript.
5. **Integration:** surviving keyframes enter the standard image queue; records map keyframe embeddings to the parent `video_id` and timestamp.

#### Re-ID & Pseudo-Labeling

Identifies individuals even when they turn away from the camera during an event:

1. **Anchor Pass:** on a high-confidence frontal face, run InsightFace; if it matches a known profile (e.g. "Bride"), record the bounding box.
2. **Body Pass:** run YOLOv10 to find "person" boxes; pass crops through OSNet for a 512-dim body embedding.
3. **Linking:** if the face/body box IoU $> 0.7$, link the body embedding to the profile for this event.
4. **Pseudo-Labeling:** for a person facing away, compare the body embedding against event-specific embeddings via cosine similarity $\text{sim}(\mathbf{u}, \mathbf{v}) = \frac{\mathbf{u} \cdot \mathbf{v}}{\|\mathbf{u}\| \|\mathbf{v}\|}$; above threshold, apply the label.

#### High-Dimensional Vector Search

Exact KNN is too slow at millions of rows: use **HNSW** indexes on the vector columns, and the inner-product operator (`<#>`) for normalized embeddings (cheaper than $L_2$ or cosine at scale).

## Validation

- **Registry lookup (unit).** Each canonical `model_id` matches exactly one inventory row; non-canonical IDs are rejected at the insert API.
- **Stale-model rejection / version bump (unit).** Swap a `model_id`/bump `model_version`; assert pre-swap entries are flagged stale and excluded from queries, and background regen replaces them per-asset (not via global truncate).
- **Embedding-provenance round-trip (unit).** Insert an embedding tagged `(model_id, model_version)`; query; assert the tuple is preserved.
- **Namespace separation (unit).** Promote an AI tag to a user tag; assert the user-tag entry has a fresh user-scoped `add_id` and the AI entry remains separately editable.
- **Inference parity across devices (smoke per platform).** Run the canonical Semantic Search model on two devices over the same fixture; assert vectors are byte-identical (quantization-permitting). On a platform that cannot reach bit-exactness, assert the platform-partition fallback engages rather than silently merging incomparable vectors.
- **Algorithm correctness (smoke).** Video-as-sparse-photos selects keyframes at expected timestamps; the Re-ID loop produces expected per-event pseudo-labels.
- **Thermal throttle / batching bound (smoke).** Past the temperature threshold the pipeline pauses and resumes after cooldown; on a low-memory testbed micro-batch sizes stay within the ceiling and OOM never occurs.

Two bounded E2E cases live in [Module Map](/design/module-map/#e2e-test-surface): index an asset → query semantically → match, and model regen after a version bump rebuilding the index.
