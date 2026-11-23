---
title: AI/ML Integrations
description: On-device AI architecture, versioned pipelines, the append-only weight registry, the merged feature map, embedding provenance, and AI/user metadata separation
audit: none
---

All ML runs **client-side**, and only after the user opts in. The stable contract is [](#ai-output-containment), [](#pipelines-and-versioning), the [](#weight-registry) and [](#embedding-provenance). The [](#feature-map) and candidate weights are defaults that will move with evaluation.

There are three categories: [](#semantic-indexing) (one global embedding per asset), [](#dense-tagging) (local embeddings for objects, faces, scenes) and [](#quality-assessment) (per-asset scores). Orchestration and the runner seam are in `capsule-core::ml`. Pipeline contracts belong in `capsule-core::ml::pipeline`, and fetching and readiness in `capsule-sdk::ml` (both **Planned**, see [](#implementation-status)). The vector index is the `capsule-core::ml::VectorIndex` port, and its SQLite adapter is `capsule-library::db`.

## Scope

The server has **no ML role**. It never runs inference, never hosts weights and ships no model in any image. It stores AI outputs only as encrypted sidecar fields and embedding-class `DerivativeManifest`s ([](/design/cryptography/provenance/#derivative-provenance)), and it checks those against the same blob standards as any other upload. Server-side content scanning is ruled out by [](/design/moderation/).

## AI Output Containment

**AI outputs live in a separate namespace from user metadata, structurally, not by policy.** [](/design/metadata/#tag-provenance-and-namespacing) owns the shape (`tags_ai` vs `tags_user`, plus AI-only sidecar fields). Consequences:

- An AI output can never overwrite a user field. Each AI facet ships in its own AI-namespaced sidecar field ([](/design/metadata/#sidecar-schema-v1)), user corrections write user fields, and AI re-runs leave them alone.
- Every AI output carries `(model_id, model_version)`. Outputs of a superseded generation ([](#generation-cutover)) are stale and are excluded from queries until regenerated.
- Promoting an AI tag to a user tag is an explicit, signed lifecycle operation ([](/design/authorization/#the-closed-action-set)), never automatic.

This blocks the "AI silently overwrites user data" damage class ([](/design/threat-model/schema-rules/#forbidden-client-behaviors)).

**AI grouping is a pure function** of `(input asset set, model_id, model_version)`. Inputs are processed in sorted asset-id order and stochastic steps use fixed seeds, so re-running yields byte-identical output. This satisfies [](/design/metadata/#grouping-convergence-requirement).

## Runtime

**ONNX is the only exported model format.** Every released weight is one ONNX file at a fixed opset, and every platform runs it through ONNX Runtime, linked from the Rust core behind the default-off `inference` feature of `capsule-core` (**Planned**: the feature exists and links nothing yet). A model is therefore exported, quantized and validated once rather than once per platform format, and one known-answer fixture checks it everywhere. The one exception is the platform text recogniser ([](#text-recognition)), which Capsule does not ship and which serves only ephemeral output.

The execution provider (EP) is chosen by **output class**, not by platform:

| Output class | Tasks | EP | Parity contract |
| --- | --- | --- | --- |
| **Deterministic** | Every stored embedding (semantic, face, re-ID), and every output that feeds a grouping, a stored tag or a stored flag: face and person boxes, zero-shot tags, the sensitive-content head | Deterministic CPU EP only: fixed thread count, deterministic kernels, quantized weights | Byte-identical to the release's known-answer output |
| **Ephemeral** | Captions, OCR text, VLM answers, transcripts, display-only boxes | Any native EP the device has (CoreML, NNAPI/QNN, XNNPACK, CUDA/TensorRT, DirectML), CPU fallback | Within the release's declared tolerance of the CPU reference |

Stored embeddings are compared across devices ([](#embedding-provenance)), and AI grouping must regroup byte-identically ([](#ai-output-containment)), so anything either of them consumes runs deterministically. Native accelerators do not promise bit-exactness, so they serve only outputs a person reads: text that is stored once by the device that produced it, carries its tuple, and is never recomputed for comparison. That is also where large models need native acceleration to have usable latency.

## Pipelines and Versioning

**Client builds version pipelines. Weights are versioned separately and never ship in a client build.**

A **pipeline** is `(task, pipeline_version)`. It fixes everything code has to agree on: input decoding and preprocessing, the ONNX input/output signature, required operators, postprocessing, output dimension and metric, and output class. It lives in `capsule-core::ml::pipeline`, compiled into every client. Changing any of those is a new `pipeline_version` and needs a client release.

A **weight release** is any ONNX file that satisfies a pipeline. It is released into the [](#weight-registry) without a client release. A client runs the newest non-yanked release, for each lineage it enables, whose `pipeline_version` it implements.

The provenance tuple maps onto this as follows:

- `model_id` is the weight **lineage**: one line of releases for one task under **one** `pipeline_version`. A new pipeline version starts a new lineage, so the tuple always implies its pipeline and no stored schema gains a field.
- `model_version` is the **release number** within the lineage, monotonic.

A client enables at most **one lineage per task**, so a task's index has one current generation ([](#generation-cutover)).

### Weight Registry

The registry is a **signed, append-only** list of every weight release Capsule has published. It is the only source of truth for which `(model_id, model_version)` tuples exist. It lives in the repository and is published with each registry change. Its records are typed in `capsule-core::ml::weights` (**Planned**).

Each release record carries:

- identity: `model_id`, `model_version`, task, `pipeline_version` (fixed for the lineage), `released_at`;
- the bytes: SHA-256, size, and one or more source URLs (GitHub Releases, Hugging Face or another public host);
- licence: the weight licence, the training datasets with their licences, and any exception granted under [](/design/licensing/#model-weights-and-datasets);
- evaluation: the evaluation sets, scores, and the comparison that justified releasing it;
- readiness: a known-answer input, its reference output, and a tolerance (for deterministic pipelines, the bound inside which a mismatching device may still run on a platform partition);
- task constants: the fixed parameters a pipeline's postprocessing reads, such as the face-clustering `min_faces` and `max_distance` ([](/design/people/#clustering));
- device floor: minimum RAM and the tier ([](#feature-map)).

Corrections and withdrawals are **new records** (`supersede`, `yank`) and never edits. The client enforces append-only behaviour: a fetched registry must extend the cached one byte for byte, and one that drops or rewrites a record is refused. This also stops a rollback to an older registry. A delegated registry key signs release records. It is named by root-signed `delegate` records in the same log, which also carries root-signed `checkpoint` records for recovery from a fork. Every client compiles in the two root public keys. [](/design/cryptography/keys/#project-release-keys) owns custody, rotation, compromise and fork recovery. A client that sees a synced AI output whose tuple its snapshot lacks refreshes the registry first. If the tuple is still unknown it refuses the output and keeps the asset's other data.

### Distribution and Readiness

- **On demand.** Weights are fetched per enabled lineage after opt-in, never bundled in an app-store binary, a server image or an installer. The weight release cycle is independent of the app stores.
- **Hash-pinned.** The bytes must match the registry record's SHA-256 whatever URL served them. A user or operator may **override the source URL** (a mirror, or an air-gapped copy), but never the hash. A different model needs a registry release.
- **Ready only after a known-answer pass.** A fetched release runs its known-answer input on this device's EP. The device then does one of three things:
  - **exact match:** the release is `ready` on the shared index;
  - **deterministic pipeline, mismatch within the record's tolerance:** the release is `ready` on a `platform` partition ([](#embedding-provenance));
  - **anything else:** the release is not `ready`, and the previous release (if any) keeps serving.

  Every outcome is logged with the EP, the release and the deviation.
- **Per-tier download.** The client offers each tier ([](#feature-map)) separately, with its disk cost shown before anything is fetched.

### Generation Cutover

A new release in a lineage opens a new index **generation**. It must not break the index a user already has:

1. The new release is fetched and passes readiness.
2. Regeneration runs in the background over originals, asset by asset, writing new-generation rows beside the old ones. Assets imported while it runs are processed by both generations.
3. The **previous generation keeps answering queries** until the new one covers every asset the old one covers.
4. Cutover is atomic per task (one lineage per task, [](#pipelines-and-versioning)). Old-generation rows are deleted only after it, never truncate-and-rebuild ([](/design/threat-model/validation/#client-side-validation-invariants)).

A single ranking or grouping never mixes generations. "Superseded" in [](#ai-output-containment) means superseded by a completed cutover.

**Status: Planned.** `capsule-core::ml::Registry` still reads a compiled-in inventory, and `capsule-core::ml::regen` excludes a superseded version as soon as it is known rather than at cutover.

Smart-album AI terms follow the current generation ([](/design/organization/#system--smart-albums-views)).

## Semantic Indexing

Image and text queries are embedded and compared by distance, and [](#embedding-provenance) governs cross-device comparability. The real ONNX Runtime runner is post-v1. v1 ships the execution path, registry, index and staleness rules against a fixture runner.

### Semantic Search

Semantic search is how a text query reaches this index. The query's free text is embedded with the **semantic embedder** ([](#feature-map)), and its nearest neighbours are taken from the canonical vectors of the current generation ([](#synced-embeddings), [](#generation-cutover)), so stale and unpublished local vectors are excluded structurally. [](/design/search/#semantic-search) owns how the result merges with lexical search.

- **On device, no network.** The query embedding and the scan run locally. A device without the embedder's weights runs lexical search only and says so; it never sends a query anywhere to be embedded.
- **A ranking, never a stored fact.** Similarity depends on the querying device's text encoder and changes with each model version, so it is never written to a sidecar, never a smart-album term, and never compared across `(model_id, model_version)` tuples ([](#embedding-provenance)).
- **Filtered before it is ranked.** Structured search terms narrow the candidate set first. When the narrowed set is small, the scan ranks exactly that set rather than taking a global top-*k* and filtering afterwards, which could return nothing.
- **Explained.** A hit that only semantic search produced is marked as a semantic match in the result.

**Status.** `semantic_search` in `capsule-core::ml` embeds the query, refuses a non-canonical runner and queries the vector index. It is exercised end to end in `capsule-library/tests/model_regen_e2e.rs`, but only against the fixture runner, which hashes its input and so answers nothing meaningful. Real semantic search therefore needs the first real runner. No app or CLI path calls it yet.

### Image Categorization & Tagging

Semantic embeddings are reused for zero-shot tagging; there is no separate scene or object classifier.

## Dense Tagging

Face detection and clustering use the face rows of the [](#feature-map). [](/design/people/) owns the clustering contract and the user's corrections. Everything in this section depends on those rows and is **Blocked** with them.

### Identity Refinement

Known people are the user's **corrections** ([](/design/people/#corrections)): names, merges, splits and "not this person", in the people section of the library-settings document, which only the owner set reads. They are user input to the clustering view, not the view itself, which stays computed ([](/design/metadata/#grouping-convergence-requirement)). A correction is anchored by an asset id and a box rather than by a vector, so a new release re-embeds the faces it names ([](/design/people/#corrections-outlive-models)). A model never learns on the device, because per-device weights would break cross-device comparability.

- **Feedback** (confirm, reject, merge, split) writes corrections, never weights. Clustering constants stay per-release, never per-person or per-user ([](/design/people/#clustering)).
- **Pseudo-labels** from high-confidence matches are AI outputs in the AI namespace, never user labels.
- **Review queue.** Unassigned faces are ordered by their distance to the nearest named person's faces, so the least certain come first.
- **Metric-learning refresh** is a new weight release like any other, and it goes through [](#generation-cutover).

### Sensitive Content

A classifier head over the semantic embedding produces a sensitivity flag, which clients use to **blur by default**. It runs on the provisioning user's client:

- **when a drop arrives**, before it is reviewed, so the review surface ([](/design/web-upload/)) already blurs it. A flagged drop raises the `drop_sensitive` local alert ([](/design/notifications/#alert-classes)) to that user, who may be on another device or reviewing later;
- **at import.**

On an adopted asset the flag is the AI-namespaced `sensitivity_ai` sidecar field ([](/design/metadata/#sidecar-schema-v1)). A user who disagrees dismisses it, which writes `flagged: false` with the same `model_id` and `model_version`. Re-runs of that release leave a dismissal alone. A later generation evaluates the asset again. The flag is advisory, never a block, and the server never sees it in plaintext.

## Text Recognition

Text in an image (signs, receipts, documents, screenshots) is recognised on device. It is the first post-v1 task to ship.

- **Runner.** The platform recogniser first (Apple Vision, ML Kit on Android), each a lineage of its own in the [](#weight-registry) whose records name the platform framework and its version in place of weight bytes; an offline ONNX recogniser, released like any other weight, gives the CLI and desktop the same output. Its output is ephemeral-class ([](#runtime)).
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

Embeddings live in local **SQLite + `sqlite-vec`** and are **derived state**. A lost index is refilled from the synced embedding derivatives ([](#synced-embeddings)) and, for assets that have none, by re-running inference over originals, the same path a new release takes.

## Embedding Provenance

Every stored embedding (local index, backup, or embedding-class `DerivativeManifest`, [](/design/cryptography/provenance/#derivative-provenance)) carries `(model_id, model_version)` naming a [](#weight-registry) record, which also fixes its pipeline. Embeddings are never compared across tuples.

- The index MUST **refuse inserts** from a `(model_id, model_version)` absent from the client's registry snapshot. Known-but-superseded releases are admitted stale-flagged (the regeneration queue) and excluded from queries.
- A new release is a new `model_version`. Regeneration writes new entries per asset beside the old ones, and the old ones are deleted only after cutover ([](#generation-cutover)).

**One canonical vector per asset.** Image embeddings are computed once and shared ([](#synced-embeddings)), so devices compare the *same* stored vector rather than needing byte-identical inference. Only the text-query encoder runs on every device, and its output is only ever compared against image vectors of the same `(model_id, model_version)`. Stored-embedding pipelines still run only on the deterministic CPU path ([](#runtime)), and a device must match the release's known-answer output exactly ([](#distribution-and-readiness)) before it may **publish** a vector. Core-tier model size is bounded by the lowest-end supported device. A device that is within tolerance but not bit-exact never publishes; its locally computed vectors carry a `platform` discriminator and are **never merged** into the canonical index: the worst case is duplicated regeneration, never wrong results (row #14 of [](/design/threat-model/scenarios/#damage-scenario--invariant-map)).

### Synced Embeddings

An image embedding is stored as an `embedding`-role derivative ([](/design/cryptography/provenance/#derivative-provenance)) carrying its `(model_id, model_version)`, sealed and chained like every other derivative. It is the asset's canonical vector for that model.

- **Fetch before compute.** The indexing queue first fetches the asset's embedding derivative for the current model. It runs inference only when none exists, or when the existing one names a model the device does not know.
- **Publish once.** A device that computes a vector, passes the known-answer check and holds the album's write tier publishes it as the first link of the `(asset_id, embedding)` chain. A device that finds a chain head already present for the same `(model_id, model_version)` discards its own vector and adopts the head; it never publishes a competing link. A model bump is a `derivative-replace` naming the new tuple, so both tuples never mix ([](#embedding-provenance)).
- **Shared with readers.** Album co-members open the derivative with the album key like any thumbnail, so a reader-tier member never re-indexes an album it can see but cannot write.
- **Faces ride the same rule.** Face boxes and face embeddings ([](#dense-tagging)) are published as derivatives of the same model-tagged form once their role is added to the closed role set, which is a `protocol_version` change ([](/design/threat-model/schema-rules/)).
- **Quota.** Embedding derivatives count as derivative bytes against the asset holder ([](/design/quota/#accounting-model)).
- **Leaks nothing new.** The server sees one more sealed blob per asset per model, of a size fixed by the model; a vector's contents are never visible to it.

## Models and Algorithms

### Feature Map

One pipeline per task. A backbone is reused across features whenever quality allows, to bound disk and peak memory. Candidate weights are **candidates**: none is final until its registry record carries an evaluation and licences verified against [](/design/licensing/#model-weights-and-datasets). That rule covers the training data too, which is where most candidates are still unverified. A named licence below is the weight licence only.

Tiers bound what a device downloads. **Core** is sized for the lowest-end supported device. **Optional** is opt-in per feature. **Large** is for desktops and high-RAM devices only. Each tier has a **download ceiling**, counting the weight bytes of the current release of every pipeline in it:

| Tier | Ceiling |
| --- | --- |
| Core | ≤ 150 MB in total |
| Optional | ≤ 500 MB per feature |
| Large | ≤ 4 GB per feature |

The publishing gate refuses to sign a release record that would push its tier over its ceiling. Clients do not re-check ceilings. A pipeline counts against the tier in its record, and a shared backbone counts once, in the lowest tier that uses it. A ceiling changes only by a change to this table, recorded in an ADR.

| Feature | Pipeline | Candidate weights | Tier | Status |
| --- | --- | --- | --- | --- |
| Text and similarity search, similar photos | Semantic embedder | SigLIP / OpenCLIP class, INT8 (MobileCLIP weights are research-only, so inadmissible) | Core | Planned |
| Scene and object tags (zero-shot) | Semantic embedder | (reused) | Core | Planned |
| Near-duplicate detection | Semantic embedder + pHash | (reused) | Core | Planned |
| Sensitive-content flag | Classifier head on the semantic embedding | Linear probe trained in-project | Core | Planned |
| Expression search ("happy faces") | Semantic embedder over face crops | (reused) | Core | **Blocked** (needs face boxes) |
| Face boxes and landmarks | Face detector | None admissible known; train in-project | Core | **Blocked** (licence) |
| Face identity, clustering | Face embedder | None admissible known; train in-project | Core | **Blocked** (licence) |
| Object and person boxes, region tags | Object detector | RT-DETR / D-FINE class (Apache-2.0) | Optional | Planned |
| Person re-ID without a face | Re-ID embedder | OSNet class | Optional | Post-v1 |
| Text recognition (OCR) | Platform recogniser, else a text recogniser ([](#text-recognition)) | Apple Vision, ML Kit; offline candidate not chosen | Optional | Post-v1 |
| Alt-text, captions | Captioner | Florence-2 class | Optional | Post-v1 |
| Chat about an image and its metadata | Small VLM | SmolVLM / Moondream class, quantized | Large | Post-v1 |
| Speech captions for video | ASR | Whisper class | Large (batch on Optional) | Post-v1 |
| Video shot boundaries | Shot detector | TransNet v2 class | Optional | Post-v1 |
| Exact duplicates | None: content hash | — | — | Implemented |
| Burst detection | None: EXIF burst id, capture-time window + pHash | — | — | Planned, as an extension of stack auto-detection ([](/design/organization/#asset-stacking)) |

**Face rows are Blocked.** The widely used face embedders and the strongest detectors (InsightFace's SCRFD and ArcFace, AdaFace, ElasticFace) ship weights or training sets licensed for non-commercial use only. Lighter detectors with permissive weight licences exist, but their training data is unverified. The planned route is to train the detector and embedder in-project on GPU from datasets whose licences pass [](/design/licensing/#model-weights-and-datasets), synthetic sets included. The block lifts when such a release passes its evaluation.

**Where Capsule deliberately does less:**

- **No expression model.** The semantic embedder over face crops answers expression queries well enough for search, at no disk cost.
- **No content-based orientation model in v1.** EXIF orientation only. A rotation classifier is a post-v1 candidate if field data shows mis-rotated imports are common.
- **No VLM chat or real-time ASR on Core-tier devices.** Their weights exceed the Core ceiling by an order of magnitude. Captions and transcripts are batch jobs on Optional devices and stored as AI outputs, so lower-tier devices still read them once synced.
- **No on-device fine-tuning** ([](#identity-refinement)).
- **No dedicated scene or object classifier.** Zero-shot tags from the embedder cover them, and detection is opt-in.

### Datasets

Datasets are used for **evaluation**, and for **training** only where no acceptable released weight exists (today, faces and the sensitive-content head). Each one a release used is named in its registry record with its licence, under the same rule as weights. Evaluation candidates, each with its licence still to verify:

- **COCO**: captions (five per image) and detection boxes, for the captioner and detector.
- **TextCaps**, **TextVQA**: captions and questions over text in images, for OCR-aware captioning.
- **VQAv2**: question answering, for the VLM.
- **VATEX**, **MSR-VTT**: video captions (VATEX is multilingual), for video captioning and ASR-adjacent work.

### Key Algorithmic Implementations

- **Video as sparse photos.** Detect scene cuts, sample frames at 10/50/90% of each scene, drop frames below a Laplacian-variance blur threshold, and run ASR on the audio track. Keyframes enter the image queue mapped to `(video_id, timestamp)`.
- **Re-ID pseudo-labelling.** A high-confidence face match labels the person box it overlaps (IoU > 0.7). That box's body embedding then labels the same person facing away within the event by cosine similarity.
- **Vector search.** `sqlite-vec` `vec0` tables with inner-product distance over normalised embeddings and a brute-force SIMD scan. At millions of rows, escalate to its ANN index or a partitioned scan.

### Pipeline Slots

`capsule-core::ml::Registry` currently carries a compiled-in inventory of four slots, which run against the fixture runner: semantic search, object detection, face detection and face recognition. The model ids it names (`mobileclip-b`, `yolov10`, and the face rows) are **fixture placeholders, not release candidates**. Several of them are inadmissible under [](/design/licensing/#model-weights-and-datasets). Once `capsule-core::ml::weights` exists, the slots become pipelines and their weights come from the [](#weight-registry).

## Implementation Status

- **Implemented:** `capsule-core::ml` (registry over the compiled-in inventory, fixture runner, orchestrator, index port, regeneration) and the `sqlite-vec` adapter in `capsule-library::db`.
- **Planned:**
  - `capsule-core::ml::pipeline`, `capsule-core::ml::weights` and `capsule-sdk::ml`;
  - the ONNX Runtime runner behind `inference`;
  - release-key delegation ([](/design/cryptography/keys/#project-release-keys)), the tier-ceiling gate, and generation cutover;
  - the `sensitivity_ai` sidecar field and the `drop_sensitive` alert class;
  - text recognition, which has no runner, no derivative role and no index;
  - synced embeddings ([](#synced-embeddings)): the index is filled only by local inference, and no device publishes or fetches an `embedding`-role derivative.

  Today `model_version` is a free string, not yet the monotonic release number.
- **Blocked:** the face rows and everything that depends on them ([](#dense-tagging), expression search, [](/design/people/)), until admissible weights exist.

## Validation

- **Registry (unit).** Each `(model_id, model_version)` maps to one registry record; unknown tuples are refused at insert.
- **Registry append-only (unit).** A fetched registry that drops, reorders or rewrites a cached record, or fails its signature, is refused, and the cached one stays.
- **Hash pin (unit).** An overridden URL serving different bytes is refused.
- **Delegation (unit).** A record is accepted only under the delegation current at its log position ([](/design/cryptography/keys/#validation)).
- **Tier ceilings (publishing gate).** A record that pushes its tier over its ceiling is refused before signing, and a shared backbone is counted once.
- **Fork recovery (unit).** A divergent registry is refused without a covering `checkpoint` and superseding `delegate`, and is adopted with both.
- **Readiness (smoke).** A release failing its known-answer check is never marked `ready`, and the previous release keeps serving.
- **Generation cutover (unit).** Queries read only the old generation until cutover and only the new one after. Old rows are deleted only after cutover.
- **Provenance round-trip (unit).** The tuple and `pipeline_version` survive insert and query.
- **Namespace separation (unit).** Promotion creates a user-scoped entry with a fresh `add_id`; the AI entry stays separate.
- **Inference parity (smoke, per platform).** Stored-embedding pipelines: the same fixture gives byte-identical vectors, or the platform partition engages. Ephemeral pipelines: every EP is within the release's tolerance of the CPU reference.
- **Synced embeddings (unit).** A present chain head for the current tuple suppresses inference; a device failing the known-answer check never publishes; a second device's vector for the same tuple is discarded, never chained; an unknown tuple triggers local inference.
- **Algorithms (smoke).** Expected keyframes and Re-ID labels.
- **Thermal / batching (smoke).** Pause past threshold, resume after cooldown, no OOM on a low-memory testbed.

End-to-end cases are in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
