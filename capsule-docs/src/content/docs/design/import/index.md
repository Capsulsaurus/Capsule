---
title: Import
description: How Capsule brings assets from external sources into a library
status: draft
---

**Import** brings assets from an external source (a filesystem directory, a camera, another photo service) under Capsule's management on one device. Bytes then reach the server through the [transfer protocol](/design/transfer/), and other devices learn of them through the [sync feed](/design/sync/).

| Page | Covers |
| --- | --- |
| [Pipeline](/design/import/pipeline/) | Scan, plan, execute; streaming mode; third-party importers |
| [Camera Import](/design/import/camera-import/) | PTP/IP cameras as a source adapter |

[Peering](/design/peering/) moves libraries device-to-device through the [backup artifact](/design/backup-recovery/), not through import.
