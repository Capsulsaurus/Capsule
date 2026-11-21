---
title: AI/ML Integrations
description: AI feature architecture, the canonical model inventory, embedding provenance, and AI/user metadata separation
audit: none
---

All ML runs **client-side**; the server never holds plaintext. The stable contract is [](#ai-output-containment), the model inventory ([](#models-and-algorithms)) and [](#embedding-provenance); feature lists and model picks are defaults that will move with field testing.

There are three categories: [](#semantic-indexing) (one global embedding per asset), [](#dense-tagging) (local embeddings for objects, faces, scenes) and [](#quality-assessment) (per-asset scores). Orchestration is `capsule-core::ml`, platform runners (CoreML, NNAPI, [ONNX Runtime](https://github.com/microsoft/onnxruntime)) are in `capsule-sdk`, and the vector index is the `capsule-core::ml::VectorIndex` port, whose SQLite adapter is `capsule-library::db`.

## AI Output Containment

**AI outputs live in a separate namespace from user metadata, structurally, not by policy.** [](/design/metadata/#tag-provenance-and-namespacing) owns the shape (`tags_ai` vs `tags_user`, plus AI-only sidecar fields). Consequences:

- An AI output can never overwrite a user field. Each AI facet ships in its own AI-namespaced sidecar field ([](/design/metadata/#sidecar-schema-v1)), user corrections write user fields, and AI re-runs leave them alone.
- Every AI output carries `(model_id, model_version)`. Outputs of a superseded model are stale and excluded from queries until regenerated.
- Promoting an AI tag to a user tag is an explicit, signed lifecycle operation ([](/design/authorization/#the-closed-action-set)), never automatic.

This blocks the "AI silently overwrites user data" damage class ([](/design/threat-model/schema-rules/#forbidden-client-behaviors)).

**AI grouping is a pure function** of `(input asset set, model_id, model_version)`. Inputs are processed in sorted asset-id order and stochastic steps use fixed seeds, so re-running yields byte-identical output, satisfying [](/design/metadata/#grouping-convergence-requirement).

## Semantic Indexing

Image and text query are embedded and compared by distance, and [](#embedding-provenance) governs cross-device comparability. The real weight-loading runner is post-v1, with weights downloaded at runtime behind the default-off `inference` feature. v1 ships the execution path, registry, index and staleness rules against a fixture runner.

### Semantic Search

Semantic search is how a text query reaches this index. The query's free text is embedded with the **Semantic Search** embedder ([](#models-and-algorithms)), and its nearest neighbours are taken from the canonical vectors of the current model version ([](#synced-embeddings)), so stale and unpublished local vectors are excluded structurally. [](/design/search/#semantic-search) owns how the result merges with lexical search.

- **On device, no network.** The query embedding and the scan run locally. A device without the embedder's weights runs lexical search only and says so; it never sends a query anywhere to be embedded.
- **A ranking, never a stored fact.** Similarity depends on the querying device's text encoder and changes with each model version, so it is never written to a sidecar, never a smart-album term, and never compared across `(model_id, model_version)` tuples ([](#embedding-provenance)).
- **Filtered before it is ranked.** Structured search terms narrow the candidate set first. When the narrowed set is small, the scan ranks exactly that set rather than taking a global top-*k* and filtering afterwards, which could return nothing.
- **Explained.** A hit that only semantic search produced is marked as a semantic match in the result.

**Status.** `semantic_search` in `capsule-core::ml` embeds the query, refuses a non-canonical runner and queries the vector index. It is exercised end to end in `capsule-library/tests/model_regen_e2e.rs`, but only against the fixture runner, which hashes its input and so answers nothing meaningful. Real semantic search therefore needs the first real runner. No app or CLI path calls it yet.

### Image Categorization & Tagging

Semantic embeddings are reused for zero-shot tagging; there is no separate classifier.

## Dense Tagging

Face detection and clustering use the **Face Detection** and **Face Recognition** inventory rows ([](#models-and-algorithms)).

## Text Recognition

Text in an image (signs, receipts, documents, screenshots) is recognised on device. It is the first post-v1 task to ship.

- **Runner.** The platform recogniser first (Apple Vision, ML Kit on Android), each with its own inventory row; an offline model gives the CLI and desktop the same output.
- **AI output.** Recognised text is never a caption or a label. It carries `(model_id, model_version)` ([](#ai-output-containment)) and travels as a model-tagged derivative under the rule in [](#synced-embeddings).
- **Search.** The text is indexed into the full-text index ([](/design/search/#indexed-axes)), so free text finds it offline.
- **Rules.** Its arrival is a trigger for import rules ([](/design/organization/#import-rules)).

## Quality Assessment

Post-v1, with its sidecar fields reserved under [](#ai-output-containment). Fixed now:

- Group-scoped evaluations (best shot within a stack, burst or similarity group) run strictly **after** grouping.
- They are keyed by `(group_id, membership_hash, model_id, model_version)`, where `membership_hash` is the hash of sorted member ids, so a membership change invalidates them by construction.
- Ties break by asset id.
- The only durable outcome is the `role = primary` pointer ([](/design/organization/#asset-stacking)).

## Model Batching

Execution mode is chosen per device from available RAM: **horizontal** (model-by-model, fewer resident models) or **vertical** (all models per asset, less I/O, OOM risk). Batching uses micro-batches of 1/4/8, INT8/FP16 quantization and a thermal pause past a threshold (e.g. 40 °C).

## Database Indexing and View Generation

Embeddings live in local **SQLite + `sqlite-vec`** and are **derived state**. A lost index is refilled from the synced embedding derivatives ([](#synced-embeddings)) and, for assets that have none, by re-running inference over originals, the same path a model bump takes.

## Embedding Provenance

Every stored embedding (local index, backup, or embedding-class `DerivativeManifest`, [](/design/cryptography/provenance/#derivative-provenance)) carries `(model_id, model_version)` naming its inventory row. Embeddings are never compared across tuples. Each `model_id` is declared in exactly one inventory row.

- The index MUST **refuse inserts** from a `(model_id, model_version)` unknown to the inventory. Known-but-superseded versions are admitted stale-flagged (the regeneration queue) and excluded from queries.
- A model swap bumps `model_version`. Regeneration replaces entries per asset, deleting old ones only after new ones persist, never truncate-and-rebuild ([](/design/threat-model/validation/#client-side-validation-invariants)).

**One canonical vector per asset.** Image embeddings are computed once and shared ([](#synced-embeddings)), so devices compare the *same* stored vector rather than needing byte-identical inference. Only the text-query encoder runs on every device, and its output is only ever compared against image vectors of the same `(model_id, model_version)`. Each device still pins a deterministic execution path (fixed operators, deterministic kernels, quantized weights) and must pass the model's known-answer check before it may **publish** a vector. A device that fails the check never publishes; its locally computed vectors carry a `platform` discriminator and are **never merged** into the canonical index: the worst case is duplicated regeneration, never wrong results (row #14 of [](/design/threat-model/scenarios/#damage-scenario--invariant-map)).

### Synced Embeddings

An image embedding is stored as an `embedding`-role derivative ([](/design/cryptography/provenance/#derivative-provenance)) carrying its `(model_id, model_version)`, sealed and chained like every other derivative. It is the asset's canonical vector for that model.

- **Fetch before compute.** The indexing queue first fetches the asset's embedding derivative for the current model. It runs inference only when none exists, or when the existing one names a model the device does not know.
- **Publish once.** A device that computes a vector, passes the known-answer check and holds the album's write tier publishes it as the first link of the `(asset_id, embedding)` chain. A device that finds a chain head already present for the same `(model_id, model_version)` discards its own vector and adopts the head; it never publishes a competing link. A model bump is a `derivative-replace` naming the new tuple, so both tuples never mix ([](#embedding-provenance)).
- **Shared with readers.** Album co-members open the derivative with the album key like any thumbnail, so a reader-tier member never re-indexes an album it can see but cannot write.
- **Faces ride the same rule.** Face boxes and face embeddings ([](#dense-tagging)) are published as derivatives of the same model-tagged form once their role is added to the closed role set, which is a `protocol_version` change ([](/design/threat-model/schema-rules/)).
- **Quota.** Embedding derivatives count as derivative bytes against the asset holder ([](/design/quota/#accounting-model)).
- **Leaks nothing new.** The server sees one more sealed blob per asset per model, of a size fixed by the model; a vector's contents are never visible to it.

## Models and Algorithms

One row per task. A backbone is reused across tasks whenever quality allows, to bound peak memory: the Semantic Search embedder also feeds zero-shot tagging and semantic de-duplication, and YOLOv10 serves object and person detection.

### v1-Committed Slots

| Task | Model | Function |
| --- | --- | --- |
| **Semantic Search** | MobileCLIP-B (ONNX, INT8); quantized SigLIP-tiny fallback | Global embedding for text and similarity search |
| **Object Detection** | YOLOv10 | Objects for dense tagging; reused for person detection |
| **Face Detection** | SCRFD | Face boxes and landmarks |
| **Face Recognition** | InsightFace (AdaFace) | Face embeddings, robust to low quality |

### Candidate Tasks (post-v1)

Each gains a full inventory row when it ships: VLM tagging/captioning (Florence-2, Qwen2.5-VL, BLIP-2); person re-ID (OSNet); scene and landmark classification (ViT-L, DINOv2); ASR (Distil-Whisper-large); quality (NIMA, blur/exposure/noise); near-duplicates (pHash + embeddings); video shot boundaries (TransNet v2); NSFW classification.

### Key Algorithmic Implementations

- **Video as sparse photos.** Detect scene cuts, sample frames at 10/50/90% of each scene, drop frames below a Laplacian-variance blur threshold, and run ASR on the audio track. Keyframes enter the image queue mapped to `(video_id, timestamp)`.
- **Re-ID pseudo-labelling.** A high-confidence face match labels the person box it overlaps (IoU > 0.7), and that body embedding (OSNet) then labels the same person facing away within the event by cosine similarity.
- **Vector search.** `sqlite-vec` `vec0` tables with inner-product distance over normalised embeddings and a brute-force SIMD scan. At millions of rows, escalate to its ANN index or a partitioned scan.

## Known Gaps

- Text recognition has no runner, no derivative role and no index.

- Synced embeddings ([](#synced-embeddings)) are not implemented: the index is filled only by local inference, and no device publishes or fetches an `embedding`-role derivative.

## Validation

- **Registry (unit).** Each `model_id` maps to one inventory row; unknown tuples are refused at insert.
- **Version bump (unit).** Pre-bump entries go stale, are excluded from queries, and are replaced per asset.
- **Provenance round-trip (unit).** The tuple survives insert and query.
- **Namespace separation (unit).** Promotion creates a user-scoped entry with a fresh `add_id`; the AI entry stays separate.
- **Inference parity (smoke, per platform).** Same fixture → byte-identical vectors, or the platform partition engages.
- **Synced embeddings (unit).** A present chain head for the current tuple suppresses inference; a device failing the known-answer check never publishes; a second device's vector for the same tuple is discarded, never chained; an unknown tuple triggers local inference.
- **Algorithms (smoke).** Expected keyframes and Re-ID labels.
- **Thermal / batching (smoke).** Pause past threshold, resume after cooldown, no OOM on a low-memory testbed.

End-to-end cases are in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
