---
title: AI/ML Integrations
description: AI feature architecture, the canonical model inventory, embedding provenance, and AI/user metadata separation
status: draft
---

All ML runs **client-side**; the server never holds plaintext. The stable contract is [AI Output Containment](#ai-output-containment), the [model inventory](#models-and-algorithms) and [Embedding Provenance](#embedding-provenance); feature lists and model picks are defaults that will move with field testing.

Three categories: [Semantic Indexing](#semantic-indexing) (one global embedding per asset), [Dense Tagging](#dense-tagging) (local embeddings for objects, faces, scenes) and [Quality Assessment](#quality-assessment) (per-asset scores). Orchestration is `capsule-core::ml`, platform runners (CoreML, NNAPI, ONNX Runtime) are in `capsule-sdk`, and the vector index is `capsule-core::db`.

## AI Output Containment

**AI outputs live in a separate namespace from user metadata — structurally, not by policy.** The shape (`tags_ai` vs `tags_user`, plus AI-only sidecar fields) is owned by [Metadata — Tag Provenance and Namespacing](/design/metadata/#tag-provenance-and-namespacing). Consequences:

- An AI output can never overwrite a user field. Each AI facet ships in its own AI-namespaced [sidecar](/design/metadata/#sidecar-schema-v1) field; user corrections write user fields, and AI re-runs leave them alone.
- Every AI output carries `(model_id, model_version)`; outputs of a superseded model are stale and excluded from queries until regenerated.
- Promoting an AI tag to a user tag is an explicit, signed [lifecycle operation](/design/authorization/#the-closed-action-set), never automatic.

This blocks the "AI silently overwrites user data" damage class ([Forbidden Client Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors)).

**AI grouping is a pure function** of `(input asset set, model_id, model_version)`: inputs are processed in sorted asset-id order and stochastic steps use fixed seeds, so re-running yields byte-identical output. This satisfies the [grouping-convergence requirement](/design/metadata/#grouping-convergence-requirement).

## Semantic Indexing

Image and text query are embedded and compared by distance; cross-device comparability is governed by [Embedding Provenance](#embedding-provenance). The real weight-loading runner is post-v1 (weights download at runtime behind the default-off `inference` feature); v1 ships the execution path, registry, index and staleness rules against a fixture runner.

### Image Categorization & Tagging

Semantic embeddings are reused for zero-shot tagging; there is no separate classifier.

## Dense Tagging

Face detection and clustering use the **Face Detection** and **Face Recognition** [inventory](#models-and-algorithms) rows.

## Quality Assessment

Post-v1; its sidecar fields are reserved under [containment](#ai-output-containment). Fixed now: group-scoped evaluations (best shot within a stack, burst or similarity group) run strictly **after** grouping and are keyed by `(group_id, membership_hash, model_id, model_version)` (`membership_hash` = hash of sorted member ids), so a membership change invalidates them by construction. Ties break by asset id. The durable outcome is only the [`role = primary` pointer](/design/organization/#asset-stacking).

## Model Batching

Execution mode is chosen per device from available RAM: **horizontal** (model-by-model; fewer resident models) or **vertical** (all models per asset; less I/O, OOM risk). Micro-batches of 1/4/8, INT8/FP16 quantization, and a thermal pause past a threshold (e.g. 40 °C).

## Database Indexing and View Generation

Embeddings live in local **SQLite + `sqlite-vec`** and are **derived state**: a lost index is rebuilt by re-running inference over originals, the same path a model bump takes.

## Embedding Provenance

Every stored embedding (local index, backup, or embedding-class [`DerivativeManifest`](/design/cryptography/provenance/#derivative-provenance)) carries `(model_id, model_version)` naming its inventory row. Embeddings are never compared across tuples. Each `model_id` is declared in exactly one inventory row.

- The index MUST **refuse inserts** from a `(model_id, model_version)` unknown to the inventory. Known-but-superseded versions are admitted stale-flagged (the regeneration queue) and excluded from queries.
- A model swap bumps `model_version`; regeneration replaces entries per asset, deleting old ones only after new ones persist — never truncate-and-rebuild. See [Client-Side Validation Invariants](/design/threat-model/validation/#client-side-validation-invariants).

**E2EE constraint.** Comparable embeddings need byte-identical inference across devices, so each device pins a deterministic execution path (fixed operators, deterministic kernels, quantized weights) and passes a known-answer check; model size is bounded by the lowest-end supported device. A device that cannot reach bit-exactness tags its embeddings with a `platform` discriminator and they are **never merged** into another platform's index — worst case is duplicated regeneration, never wrong results ([Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) row #14).

## Models and Algorithms

One row per task; a backbone is reused across tasks whenever quality allows (the Semantic Search embedder also feeds zero-shot tagging and semantic de-duplication; YOLOv10 serves object and person detection) to bound peak memory.

### v1-Committed Slots

| Task | Model | Function |
| --- | --- | --- |
| **Semantic Search** | MobileCLIP-B (ONNX, INT8); quantized SigLIP-tiny fallback | Global embedding for text and similarity search |
| **Object Detection** | YOLOv10 | Objects for dense tagging; reused for person detection |
| **Face Detection** | SCRFD | Face boxes and landmarks |
| **Face Recognition** | InsightFace (AdaFace) | Face embeddings, robust to low quality |

### Candidate Tasks (post-v1)

Each gains a full inventory row when it ships: VLM tagging/OCR/captioning (Florence-2, Qwen2.5-VL, BLIP-2); person re-ID (OSNet); scene and landmark classification (ViT-L, DINOv2); ASR (Distil-Whisper-large); quality (NIMA, blur/exposure/noise); near-duplicates (pHash + embeddings); video shot boundaries (TransNet v2); NSFW classification.

### Key Algorithmic Implementations

- **Video as sparse photos.** Scene-cut detection, sample frames at 10/50/90% of each scene, drop frames below a Laplacian-variance blur threshold, run ASR on the audio track; keyframes enter the image queue mapped to `(video_id, timestamp)`.
- **Re-ID pseudo-labelling.** A high-confidence face match labels the person box it overlaps (IoU > 0.7); that body embedding (OSNet) then labels the same person facing away within the event by cosine similarity.
- **Vector search.** `sqlite-vec` `vec0` tables, inner-product distance over normalised embeddings, brute-force SIMD scan; at millions of rows, escalate to its ANN index or a partitioned scan.

## Validation

- **Registry (unit).** Each `model_id` maps to one inventory row; unknown tuples are refused at insert.
- **Version bump (unit).** Pre-bump entries go stale, are excluded from queries, and are replaced per asset.
- **Provenance round-trip (unit).** The tuple survives insert and query.
- **Namespace separation (unit).** Promotion creates a user-scoped entry with a fresh `add_id`; the AI entry stays separate.
- **Inference parity (smoke, per platform).** Same fixture → byte-identical vectors, or the platform partition engages.
- **Algorithms (smoke).** Expected keyframes and Re-ID labels.
- **Thermal / batching (smoke).** Pause past threshold, resume after cooldown, no OOM on a low-memory testbed.

End-to-end cases are in [Module Map](/design/module-map/#e2e-test-surface).
