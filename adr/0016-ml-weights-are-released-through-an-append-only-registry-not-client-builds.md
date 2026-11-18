# ADR-0016 — ML weights are released through an append-only registry, not client builds

- **Status:** accepted
- **Date:** 2026-11-18
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [AI/ML Integrations — Pipelines and Versioning](../capsule-docs/src/content/docs/design/ai.md#pipelines-and-versioning), [Licensing — Model Weights and Datasets](../capsule-docs/src/content/docs/design/licensing.md#model-weights-and-datasets)
- **Issues:** #1276

## Context

The ML contract named a fixed v1 inventory of four models (MobileCLIP-B, YOLOv10, SCRFD and
InsightFace/AdaFace). Each was a const row in `capsule-core::ml::Registry`, so changing a model meant
shipping a client build. App-store release cycles are slow, and weights need to improve more often
than clients ship.

A superseded version's entries were excluded from queries as soon as the new version was known.
Every model swap therefore degraded search until regeneration finished.

Several picks were not admissible as published weights:

- YOLOv10 (THU-MIG) is AGPL-3.0, inherited from Ultralytics.
- MobileCLIP's weights are research-only.
- The InsightFace detector and recognizer weights are non-commercial, and so are the large face
  training sets.

Nothing in the contract had weighed weight or dataset licences.

Design discussion before this change (not recorded in the repository) also proposed:

- server-side inference in hardware-specific server images;
- native runtimes per platform (CoreML, TFLite, TensorRT);
- a model manifest compiled into each client;
- LLaVA for chat about images.

## Decision

- Client builds version **pipelines** `(task, pipeline_version)`. Weights are released
  independently into a **signed, append-only registry** with full metadata (hashes, sources,
  licences, training data, evaluations, known-answer fixtures). Clients pick the newest
  compatible release and refuse a registry that does not extend the one they hold.
- A new release opens a new index **generation**. The old generation keeps serving queries until
  the new one covers the library, then cuts over atomically.
- **ONNX is the one exported format, run through ONNX Runtime from the Rust core.** Stored-embedding
  pipelines run only on a deterministic CPU path, while ephemeral outputs may use native execution
  providers within a tolerance.
- Weights are fetched on demand, pinned by hash, with a URL override but no hash override. They are
  ready only after a known-answer pass, and never ship in app stores or server images.
- Weight and dataset licences are gated: no non-commercial terms, and no hard copyleft unless the
  closest permitted alternative is much worse. Training in-project on GPU is the fallback.

Rejected:

- **Server-side inference.** The server holds no plaintext ([Moderation](../capsule-docs/src/content/docs/design/moderation.md)), and a server image carrying models only adds size and startup cost to a component that cannot use them.
- **Native runtimes per platform.** CoreML, LiteRT and TensorRT are fastest on their own hardware but are not bit-exact with one another, so stored embeddings would split per platform by default. They would also need three or more conversion and validation paths per release. Native acceleration survives as ONNX Runtime execution providers for ephemeral outputs only.
- **A model manifest compiled into each client.** It keeps weights tied to the client release cycle, which is the problem being solved.
- **On-device fine-tuning** for identity refinement. Per-device weights break cross-device comparability. Feedback edits user-scoped prototypes instead.

## Consequences

- The face rows are **Blocked** until an admissible face detector and embedder are trained or found.
- YOLO is replaced by an Apache-2.0 detector candidate, MobileCLIP by SigLIP/OpenCLIP-class
  candidates, and LLaVA by small permissively licensed VLMs. Training-data licences for all of
  them are still to verify.
- The compiled-in inventory and stale-on-release behaviour in `capsule-core::ml` no longer match the
  contract. The code half is #1276. Until it lands, the inventory's model ids are fixture
  placeholders.
- No gate reads the registry's licence fields yet, so review is the only check.
