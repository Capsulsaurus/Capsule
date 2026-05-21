---
title: Asset Stacking
description: Details on how asset stacking works in Capsule.
---

## Asset Stacking in Capsule

In large media collections, it’s common for related files to belong together. Instead of cluttering your library with dozens of nearly identical files, Capsule "stacks" them into a single unit.

You’ve likely seen this in action before—think of how photo apps group RAW+JPG pairs or how video editors sync external audio with camera footage. Capsule uses a "best-effort" auto-detection system to identify these relationships and keep your workspace clean.

### Photography & Mobile Stacks

* **RAW + JPEG Pairs:** The classic "prosumer" stack. We treat the uncompressed RAW file and the processed JPEG as one asset to keep your grid tidy.
* **Burst Stacks:** A sequence of high-speed stills (e.g., 10–30 fps). The app identifies a "Best Photo" and tucks the rest behind it.
* **Live Photos:** A JPEG or HEIC paired with a 1.5–3 second video clip, managed as a single interactive unit.
* **Portrait/Depth Stacks:** An image paired with its depth map. This allows you to adjust the bokeh (background blur) after the shot is taken.
* **Smart Selection:** AI-driven grouping of visually similar images taken within seconds of each other to reduce "clutter."

### Technical & Creative Stacks

* **Exposure Bracketing (HDR):** Multiple shots of the same scene at different exposure levels (e.g., -2, 0, +2 EV) to be merged into a single High Dynamic Range image.
* **Focus Stacks:** A series of shots with shifting focus points. Often used in macro photography to create "infinite" depth of field.
* **Pixel Shift Stacks:** Found in high-end mirrorless cameras. The sensor moves slightly to capture multiple shots, which are stacked for ultra-high resolution and perfect color.
* **Panorama (Stitched):** A sequence of horizontal or vertical shots intended to be merged into a single wide-field image.

### Video & Audio Stacks

* **Proxy/Optimized Stacks:** Pairs a heavy "Master" file (like 8K RAW) with a lightweight "Proxy" (like 1080p ProRes) for smoother editing performance.
* **Chaptered Video:** Action cameras (like GoPro) often split long recordings into 4GB chunks. We stack files like `GOPR001.mp4` and `GOPR002.mp4` so they appear as one continuous video.
* **Dual-System Audio:** Groups video files with high-quality external audio (WAV/AIFF) using timecode or waveform matching.
