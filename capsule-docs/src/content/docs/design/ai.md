---
title: AI/ML Integrations in Capsule
description: How do AI features fit into Capsule's architecture and design principles?
---

> **Status:** Details below are **provisional** pending experimentation. The structure of categories, the namespace separation in [AI Output Containment](#ai-output-containment), and the canonical-model invariant from [ML Models — Embedding Provenance](/design/ml-models/#embedding-provenance) are stable; the specific feature list and per-feature behavior may evolve.

Capsule runs a hierarchy of ML models for various tasks. The E2E nature of Capsule's architecture requires careful consideration of device capabilities and latency requirements for different features. We broadly categorize the AI/ML processing into three functions:

- **[Semantic Indexing](#semantic-indexing):** Generate a *global* embedding for each asset to enable natural language search and similarity search.
- **[Dense Tagging](#dense-tagging):** Generate *local* embeddings for objects, faces, and background elements to enable granular search and auto-album generation.
- **[Quality Assessment](#quality-assessment):** Generate quality scores for each asset to enable quality-based filtering and sorting.

Additional AI/ML categories may be added; the canonical inventory is [ML Models](/design/ml-models/).

## AI Output Containment

AI inference can be wrong, biased, or hallucinatory. A core design rule prevents AI output from corrupting user intent: **AI outputs land in a separate namespace from user-authored metadata, structurally, not by policy.**

- AI-suggested tags live in `tags_ai` (a separate OR-set from `tags_user`) — see [Metadata — Tag Provenance and Namespacing](/design/metadata/#tag-provenance-and-namespacing). An AI tag can never overwrite a user tag because they are different fields.
- AI-derived face identities, scene labels, and quality scores live in distinct sidecar fields (e.g. `ai_face_labels`, `ai_scene`, `ai_quality_score`) that the user does not directly edit; user corrections write to *user* fields and AI re-runs leave the user fields alone.
- Every AI output entry carries `model_id` and `model_version` (see [ML Models — Embedding Provenance](/design/ml-models/#embedding-provenance)). When the canonical model for that slot changes, old AI outputs are flagged as stale and excluded from queries until regenerated.
- Promoting an AI tag to a user tag is an explicit, signed lifecycle operation — never automatic, never silent. See [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set).

A hallucinating model can pollute its own namespace; it cannot pollute user intent. This is the structural defense against the "AI mistake silently overwrites user-authored data" damage class — see [Threat Model — Forbidden Client Behaviors](/design/threat-model/#forbidden-client-behaviors).

## Semantic Indexing

To do semantic search, you convert an image and a text query into arrays of numbers (vectors) and measure the distance between them. Every embedding model maps the universe differently, and Capsule is end-to-end encrypted, so every device must run the *same* embedding model — vectors are otherwise incomparable across devices. The canonical model for this slot is declared in [ML Models](/design/ml-models/) (see the **Semantic Search** row).

### Image Categorization & Tagging

We reuse the semantic embeddings for zero-shot classification to generate tags. This enables faceted search and auto-album generation without a separate classifier model.

## Dense Tagging

We have the following ordering of operations:

- Face Detection & Matching (Clustering): see the **Face Detection** and **Face Recognition** rows in [ML Models](/design/ml-models/). The chosen detector and embedder are SOTA-small models that run near-instantly on mobile devices.

<!-- Segmentation (SAM): The output is Coordinates (bounding boxes, polygons, or binary masks). MobileSAM and SAM-Huge just output different levels of accuracy for pixel coordinates. The database parses the coordinates identically. -->

## Quality Assessment

TODO

## Model Batching

Memory is at a premium in mobile devices. We want to be as power-efficient as possible while fulfilling the computational needs of the models. As such, we batch the execution of models in the following ways:

- Horizontal Batching (model-by-model): Run each model sequentially across all assets. This minimizes the number of models that need to be loaded in memory at once but it incurs lots of IO (since you are reading assets multiple times).
- Vertical Batching (end-to-end): Run all models at once for each asset. This minimizes IO but it is memory intensive since you need to load all models at once, and may result in OOM killing the application process (on mobile OSes).

We pick the execution model with the following process:

- Calculate RAM capacity upfront: Upon starting the task, check the device's available memory. Decide dynamically whether to use Horizontal or Vertical batching based on the device's resources.
- Enforce Micro-Batching: Never pass a massive batch to the inference engine. Break your "huge batch" down into micro-batches of 1, 4, or 8 images. This keeps the NPU cache hot and prevents battery-draining DRAM fetches.
- Quantize everything: Ensure your models are quantized to INT8 or FP16. This halves the memory bandwidth required, which directly translates to less battery consumed and less heat generated.
- Throttle based on thermals: Modern mobile APIs allow you to monitor device temperature. If the device hits 40°C, artificially pause the pipeline for a few seconds. A slightly slower job is better than the OS terminating your app or the hardware thermal-throttling your speeds to a crawl.

## Database Indexing and View Generation

Since each model (except for a few) generate embeddings in a common vector space, we store them locally in a database. We use SQLite + `sqlite-vec`.

## Models and Algorithms

The concrete model chosen for each task, and the key algorithms that combine them, are catalogued in [ML Models and Algorithms](/design/ml-models/).
