---
title: ML Models and Algorithms
description: The model inventory and key algorithmic implementations behind Capsule's AI features
---

This is the reference companion to [AI/ML Integrations](/design/ai/): the
concrete model chosen for each task, and the key algorithms that combine them.

**This doc is the canonical model inventory.** Per the [single-source-of-truth rule](/design/principles/#single-source-of-truth), every ML model identity Capsule uses is declared here and referenced from other docs by link. Swapping a model is a one-row edit in the table below.

> **Status:** The table below is **provisional** pending experimentation and field testing on Capsule's target devices in 2026. The doc *structure* — one canonical row per task with an explicit `model_id`/`model_version` — is the stable contract; the specific row choices are subject to revision and individual rows may be marked WIP or alt as the inventory matures.

**E2EE constraint on embedding models.** Capsule's server never holds plaintext, so embeddings are generated client-side. Every device that ingests assets must therefore run the *same* embedding model — otherwise vectors aren't comparable across devices. The model size floor is set by the lowest-end device Capsule supports, not by what runs comfortably on a desktop.

## Embedding Provenance

Every embedding stored in Capsule — locally in the SQLite vector index, in an encrypted backup, or inside a [`DerivativeManifest`](/design/cryptography/#derivative-provenance) for an embedding-class derivative — carries the tuple `(model_id, model_version)` identifying which row of the table below produced it. Embeddings are not comparable across `(model_id, model_version)` pairs: the vector spaces are different. The invariant:

- The vector index **refuses inserts** whose `model_id` is not the current canonical row for its task (the row marked `WIP (high priority)` or its successor). A buggy or new client uploading embeddings from an unrecognized model is rejected at the insert API, never silently mixed in.
- A model swap (a new row replacing an old one) increments `model_version` for that task. Old embeddings are **flagged as stale** and excluded from queries until they are regenerated from the originals. Cross-version semantic comparison is forbidden — see [Threat Model — Client-Side Validation Invariants](/design/threat-model/#client-side-validation-invariants).
- Regenerating embeddings after a model swap is a background task that walks the library and produces fresh embeddings at the new `model_version`. The old entries are removed only after the new ones are persisted (atomicity: per-asset replace, not a global truncate-and-rebuild).
- The mapping from `model_id` to a row in this table is what gives a swap its *single-doc-edit* property: changing the canonical model is a one-row edit here, the `model_id` string changes, and every downstream consumer follows.

This invariant lives in [Threat Model — § Damage Scenario Map](/design/threat-model/#damage-scenario--invariant-map) row #14 and is what defeats the "silent invalidation of the vector index" damage class identified in the audit.

## Specific ML Tasks & Models

<!-- TODO: break down these models into the corresponding sections of the AI/ML doc -->

<!-- TODO: Combine models here where possible (to minimize VRAM overhead) (need to consider size-accuracy tradeoff) -->
<!-- TODO: Revise this section based on experimentation/results -->

| Task                              | Category         | Model(s)                                                                                    | Dataset(s)                  | Function                                                                                                                                                                          | Implementation Status |
| --------------------------------- | ---------------- | ------------------------------------------------------------------------------------------- | --------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------- |
| **Semantic Search**               | Natural Language | **MobileCLIP-B** (ONNX, INT8) — canonical; quantized SigLIP-tiny as fallback[^semantic-alt] |                             | Generates global image embeddings for natural language search. Sized for the lowest-end device Capsule supports (see the E2EE constraint above).                                  | WIP (high priority)   |
| **Dense Tagging & OCR**           | Dense Tagging    | Florence-2                                                                                  |                             | Unified vision-language model for bounding boxes, dense captions, and reading text.                                                                                               |
| **VLM / Image Chat**              | Natural Language | Qwen2.5-VL or LLaVA-1.6                                                                     |                             | Quantized models for on-demand conversational queries about an image.                                                                                                             |
| **Image Captioning**              | Natural Language | BLIP-2                                                                                      |                             | Generates a natural language description of the image content.                                                                                                                    |
| **Face Detection**                | People           | SCRFD                                                                                       |                             | Highly efficient face bounding box and landmark detection.                                                                                                                        | WIP (high priority)   |
| **Face Recognition**              | People           | InsightFace (AdaFace)                                                                       |                             | Generates face embeddings. AdaFace excels at handling low-quality/dark images.                                                                                                    | WIP (high priority)   |
| **Person Detection**              | People           | YOLOv10                                                                                     |                             | Object detection for identifying "person" bounding boxes.                                                                                                                         |
| **Person Re-ID**                  | People           | OSNet or TorReID                                                                            |                             | Generates embeddings based on clothing and body shape when faces are hidden.                                                                                                      |
| **Expression Analysis**           | People           | EmotioNet                                                                                   |                             | Detects facial action units to infer emotions.                                                                                                                                    |
| **Quality Scoring**               | People           | LIQE / TOPIQ                                                                                |                             | Blind image quality assessment for noise, blur, and lighting without a reference image.                                                                                           |
| **Object Detection**              | Scene            | **YOLOv10**[^objdet-alt]                                                                    |                             | Detects objects and background elements for dense tagging.                                                                                                                        | WIP (high priority)   |
| **Scene Classification**          | Scene            | VIT-L, ConvNeXt-L                                                                           | Places365, SUN397           | Classifies the overall scene (e.g., "beach", "wedding", "cityscape").                                                                                                             |
| **Landmark Detection**            | Scene            | DINOv2 + GeM pooling                                                                        | Google Landmarks v2         | Detects key landmarks (e.g., Eiffel Tower, Golden Gate Bridge) for geotagging.                                                                                                    |
| **Bird/plant Detection**          | Scene            | BioCLIP                                                                                     | iNaturalist 2021            | Identifies and classifies birds and plants within images.                                                                                                                         |
| **General Animal Detection**      | Scene            | YOLOv8 finetuned on Open Images Animals                                                     | Open Images Animals         | Detects common animals (dogs, cats, horses) for tagging and search.                                                                                                               |
| **OCR**                           | Text             | TrOCR                                                                                       | SynthText, IIIT-5K          | Extracts text from images, including handwriting and signage.                                                                                                                     |
| **Screenshot Detection**          | Scene            | Custom CNN classifier                                                                       |                             | Identifies screenshots to help culling.                                                                                                                                           |
| **Voice Transcription**           | Audio            | **Distil-Whisper-large**[^asr-alt]                                                          |                             | Speech recognition for generating transcripts from video audio tracks. ~6× faster than Whisper-large-v3 at ~1% WER cost — the trade is the right one for on-device transcription. |
| **Aesthetic Scoring**             | Quality          | NIMA (Efficientnet head)                                                                    | AVA Dataset                 | Rates the aesthetic quality of images to help users find their best shots.                                                                                                        |
| **Blur detection**                | Quality          | Laplacian variance + CNN regressor                                                          | DefocusNet, CUHK            | Detect blurry images.                                                                                                                                                             |
| **Exposure Assessment**           | Quality          | Custom CNN regressor                                                                        | Custom                      | Evaluates the exposure level of images to ensure optimal lighting conditions.                                                                                                     |
| **Noise Estimation**              | Quality          | Custom CNN regressor                                                                        | Custom                      | Estimates the noise level in images to help users identify and filter out noisy shots.                                                                                            |
| **Near-duplicate / burst**        | Similarity       | pHash/dHash + CNN                                                                           | Custom                      | Same moment, slightly different                                                                                                                                                   |
| **Semantic new-duplicate**        | Similarity       | Embeddings from the canonical Semantic Search row + ANN                                     | Custom                      | Same subject, different angle/day                                                                                                                                                 |
| **Best-shot selection**           | Similarity       | Quality models combined?                                                                    | Custom                      | Select sharpest/best-exposed from burst                                                                                                                                           |
| **Shot/scene boundary detection** | Video            | TransNet v2, PyScene Detect                                                                 | BBC Planet Earth, ClipShots | Segment video for thumbnail/highlights                                                                                                                                            |
| **Highlight extraction**          | Video            | Temporal attention + quality scroe                                                          | SumMe, TVSum                | Extract best moments from videos for highlights and thumbnails.                                                                                                                   |
| **Action/activity recognition**   | Video            | VideoMAE, TimeSformer                                                                       | Kinetics-700, ActivityNet   | Sports, cooking, playing, travel                                                                                                                                                  |
| **NSFW Detection**                | Categorization   | OpenCLIP or custom CNN                                                                      | NSFW datasets               | Detects explicit content to help users filter and manage sensitive media.                                                                                                         |
| **Violence / Graphic Content**    | Categorization   | ViT classifier                                                                              | Custom                      | Detects and flags sensitive content (e.g. in shared albums)                                                                                                                       |

[^semantic-alt]: Considered and rejected: SigLIP-so400m (~400M params, impractical on the lowest-end mobile we support — the E2EE constraint forces every device to run the same model), full CLIP ViT-L/14 (similar size class), OpenCLIP ViT-G (much larger). MobileCLIP-B is the size sweet spot; quantized SigLIP-tiny stays as a fallback if MobileCLIP semantic quality is insufficient in field tests.
[^objdet-alt]: Considered and rejected for the *committed* slot: Grounding DINO (open-vocabulary; heavier; revisit if dense-tagging breadth becomes the bottleneck), RT-DETR (transformer-based; comparable accuracy, slower on mobile). YOLOv10 is the committed choice; alternatives may run as additional specialized passes later.
[^asr-alt]: Considered and rejected: Whisper-large-v3 (best accuracy but too slow on mobile for opportunistic background transcription), Whisper-medium (similar speed to Distil-Whisper-large but worse accuracy), faster-whisper CT2 ports (a runtime optimization layer; can be applied on top of Distil-Whisper).

## Key Algorithmic Implementations

<!-- TODO: There are details for several other algorithms that could be expanded here -->

### Video-as-Sparse-Photos Algorithm

Processing every frame of a video through heavy ML models is computationally prohibitive. This algorithm treats video as a sparse collection of keyframes.

1. **Cut Detection:** Use PySceneDetect (Content-Aware routing) to chunk the video into visually distinct scenes.
2. **Temporal Sampling:** Extract frames at the 10%, 50%, and 90% timestamps of each scene.
3. **Blur Rejection:** Calculate the variance of the Laplacian for each extracted frame: 

    $$V = \text{var}(\nabla^2 I)$$

. If $V$ is below a defined threshold, the frame is too blurry and is discarded.
4. **Audio Processing:** Run the canonical ASR model (see the **Voice Transcription** row above) concurrently to generate a timestamped transcript.
5. **Integration:** The surviving keyframes are pushed into the standard image-processing queue. Database records map the keyframe embeddings to the parent `video_id` and specific timestamp.

### The Re-ID & Pseudo-Labeling Loop

This algorithm identifies individuals even when they turn away from the camera during an event.

1. **The Anchor Pass:** When an image contains a high-confidence frontal face, run InsightFace. If the embedding matches a known profile (e.g., "Bride"), record the bounding box.
2. **The Body Pass:** Run a standard object detector (YOLOv10) to find all "person" bounding boxes. Pass these crops through OSNet to get a 512-dimensional body embedding.
3. **The Linking Phase:** Calculate the Intersection over Union (IoU) of the Face bounding box and the Body bounding box. If $\text{IoU} > 0.7$, link the OSNet body embedding to the "Bride" profile for the duration of this specific album/event.
4. **Pseudo-Labeling:** When an image features a person facing away (no face detected), compare the OSNet body embedding against the temporary event-specific body embeddings using cosine similarity: 

    $$\text{sim}(\mathbf{u}, \mathbf{v}) = \frac{\mathbf{u} \cdot \mathbf{v}}{\|\mathbf{u}\| \|\mathbf{v}\|}$$

. If the similarity exceeds the threshold, tag the individual as the "Bride."

### High-Dimensional Vector Search in Postgres

To maintain high throughput in Postgres, exact K-Nearest Neighbors (KNN) is too slow for millions of rows.

1. Implement **HNSW (Hierarchical Navigable Small World)** indexes on the `pgvector` columns.
2. Use the inner product operator (`<#>`) for normalized embeddings, as it is computationally cheaper than calculating $L_2$ distance (`<->`) or cosine distance (`<=>`) at scale.
