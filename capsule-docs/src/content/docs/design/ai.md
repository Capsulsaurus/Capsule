---
title: AI/ML Integrations in Capsule
description: How do AI features fit into Capsule' architecture and design principles?
---

<!-- TODO: Finalize the designs described in this article -->

## System Architecture Overview

The platform utilizes an asynchronous, event-driven microservice architecture designed to handle high-throughput ingestion of RAW photos and 4K+ video.

* **API Gateway & Core Logic:** Rust (Axum/Actix-web) for maximum throughput and memory safety.
* **Source of Truth:** PostgreSQL.
* **Vector Database:** PostgreSQL with the `pgvector` extension for storing and querying ML embeddings.
* **Message Broker & Caching:** Valkey (Stream data structures for event queuing).
* **Object Storage:** S3-compatible store (MinIO/AWS S3) for original files and generated proxies.
* **AI Inference Workers:** Python/C++ microservices running models optimized with TensorRT, ONNX Runtime, or vLLM.

## The Complete ML Pipeline

The pipeline is split into a synchronous "Fast Path" for immediate user feedback and an asynchronous "AI Path" for deep indexing.

### Phase 1: Ingestion & Fast Path

1. **Upload:** Client pushes the media file to the Object Store and notifies the Rust API.
2. **Metadata Extraction:** Rust extracts EXIF/IPTC data (f-stop, shutter speed, camera model, GPS).
3. **Deterministic Deduplication:** Rust calculates a file hash (XXH3) and a Perceptual Hash (pHash) for photos.
4. **Proxy Generation:** Rust generates web-optimized proxies and thumbnails.
5. **Event Dispatch:** A message (e.g., `media_ready_for_ai: {media_id: uuid}`) is pushed to a Valkey Stream.

### Phase 2: AI Processing (Asynchronous)

Worker nodes consume the Valkey stream and process the media in parallel:

1. **Embedding Generation:** The image is passed through a vision encoder to create a global semantic vector.
2. **Dense Tagging & OCR:** The image is analyzed for granular objects, background elements, and text.
3. **Biometric Pipeline:** Faces are detected, aligned, cropped, and embedded. Bodies are detected and embedded for Person Re-Identification (Re-ID).
4. **Quality Assessment:** The image is scored for technical flaws (blur, noise, exposure).

### Phase 3: Storage & Indexing

1. **Vector Storage:** Embeddings are written to `pgvector` columns.
2. **Graph Linking:** Re-ID embeddings are linked to specific face profiles via database relations.

## Specific ML Tasks & Models

<!-- TODO: Combine models here where possible (to minimize VRAM overhead) (need to consider size-accuracy tradeoff) -->
<!-- TODO: Revise this section based on experimentation/results -->

| Task                              | Category         | Model(s)                                | Dataset(s)                  | Function                                                                                | Implementation Status |
| --------------------------------- | ---------------- | --------------------------------------- | --------------------------- | --------------------------------------------------------------------------------------- | --------------------- |
| **Semantic Search**               | Natural Language | SigLIP (`siglip-so400m`)                |                             | Generates global image embeddings for natural language search.                          | WIP (high priority)   |
| **Dense Tagging & OCR**           | Dense Tagging    | Florence-2                              |                             | Unified vision-language model for bounding boxes, dense captions, and reading text.     |
| **VLM / Image Chat**              | Natural Language | Qwen2.5-VL or LLaVA-1.6                 |                             | Quantized models for on-demand conversational queries about an image.                   |
| **Image Captioning**              | Natural Language | BLIP-2                                  |                             | Generates a natural language description of the image content.                          |
| **Face Detection**                | People           | SCRFD                                   |                             | Highly efficient face bounding box and landmark detection.                              | WIP (high priority)   |
| **Face Recognition**              | People           | InsightFace (AdaFace)                   |                             | Generates face embeddings. AdaFace excels at handling low-quality/dark images.          | WIP (high priority)   |
| **Person Detection**              | People           | YOLOv10                                 |                             | Object detection for identifying "person" bounding boxes.                               |
| **Person Re-ID**                  | People           | OSNet or TorReID                        |                             | Generates embeddings based on clothing and body shape when faces are hidden.            |
| **Expression Analysis**           | People           | EmotioNet                               |                             | Detects facial action units to infer emotions.                                          |
| **Quality Scoring**               | People           | LIQE / TOPIQ                            |                             | Blind image quality assessment for noise, blur, and lighting without a reference image. |
| **Object Detection**              | Scene            | YOLOv10, Grounding DINO, RT-DETR        |                             | Detects objects and background elements for dense tagging.                              | WIP (high priority)   |
| **Scene Classification**          | Scene            | VIT-L, ConvNeXt-L                       | Places365, SUN397           | Classifies the overall scene (e.g., "beach", "wedding", "cityscape").                   |
| **Landmark Detection**            | Scene            | DINOv2 + GeM pooling                    | Google Landmarks v2         | Detects key landmarks (e.g., Eiffel Tower, Golden Gate Bridge) for geotagging.          |
| **Bird/plant Detection**          | Scene            | BioCLIP                                 | iNaturalist 2021            | Identifies and classifies birds and plants within images.                               |
| **General Animal Detection**      | Scene            | YOLOv8 finetuned on Open Images Animals | Open Images Animals         | Detects common animals (dogs, cats, horses) for tagging and search.                     |
| **OCR**                           | Text             | TrOCR                                   | SynthText, IIIT-5K          | Extracts text from images, including handwriting and signage.                           |
| **Screenshot Detection**          | Scene            | Custom CNN classifier                   |                             | Identifies screenshots to help culling.                                                 |
| **Voice Transcription**           | Audio            | Whisper-large-v3                        |                             | State-of-the-art speech recognition for generating transcripts from video audio tracks. |
| **Aesthetic Scoring**             | Quality          | NIMA (Efficientnet head)                | AVA Dataset                 | Rates the aesthetic quality of images to help users find their best shots.              |
| **Blur detection**                | Quality          | Laplacian variance + CNN regressor      | DefocusNet, CUHK            | Detect blurry images.                                                                   |
| **Exposure Assessment**           | Quality          | Custom CNN regressor                    | Custom                      | Evaluates the exposure level of images to ensure optimal lighting conditions.           |
| **Noise Estimation**              | Quality          | Custom CNN regressor                    | Custom                      | Estimates the noise level in images to help users identify and filter out noisy shots.  |
| **Near-duplicate / burst**        | Similarity       | pHash/dHash + CNN                       | Custom                      | Same moment, slightly different                                                         |
| **Semantic new-duplicate**        | Similarity       | SigLIP, CLIP embeddings + ANN           | Custom                      | Same subject, different angle/day                                                       |
| **Best-shot selection**           | Similarity       | Quality models combined?                | Custom                      | Select sharpest/best-exposed from burst                                                 |
| **Shot/scene boundary detection** | Video            | TransNet v2, PyScene Detect             | BBC Planet Earth, ClipShots | Segment video for thumbnail/highlights                                                  |
| **Highlight extraction**          | Video            | Temporal attention + quality scroe      | SumMe, TVSum                | Extract best moments from videos for highlights and thumbnails.                         |
| **Action/activity recognition**   | Video            | VideoMAE, TimeSformer                   | Kinetics-700, ActivityNet   | Sports, cooking, playing, travel                                                        |
| **NSFW Detection**                | Categorization   | OpenCLIP or custom CNN                  | NSFW datasets               | Detects explicit content to help users filter and manage sensitive media.               |
| **Violence / Graphic Content**    | Categorization   | ViT classifier                          | Custom                      | Detects and flags sensitive content (e.g. in shared albums)                             |

## Extended Detail: Key Algorithmic Implementations

<!-- TODO: There are details for several other algorithms that could be expanded here -->

### Video-as-Sparse-Photos Algorithm

Processing every frame of a video through heavy ML models is computationally prohibitive. This algorithm treats video as a sparse collection of keyframes.

1. **Cut Detection:** Use PySceneDetect (Content-Aware routing) to chunk the video into visually distinct scenes.
2. **Temporal Sampling:** Extract frames at the 10%, 50%, and 90% timestamps of each scene.
3. **Blur Rejection:** Calculate the variance of the Laplacian for each extracted frame: 

    $$V = \text{var}(\nabla^2 I)$$

. If $V$ is below a defined threshold, the frame is too blurry and is discarded.
4. **Audio Processing:** Run Whisper-large-v3 concurrently to generate a timestamped transcript.
5. **Integration:** The surviving keyframes are pushed into the standard image Valkey stream. Database records map the keyframe embeddings to the parent `video_id` and specific timestamp.

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
