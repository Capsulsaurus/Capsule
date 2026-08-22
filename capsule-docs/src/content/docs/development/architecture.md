---
title: Architecture
description: How Capsule combines gRPC, REST/OpenAPI, and a TUS-style resumable upload into one server surface, and which clients consume it.
---

Capsule is a cross-platform photo service designed for professional and enthusiast photographers who expect fast syncing, seamless uploads, and powerful search—regardless of device or network conditions.

This page is the orientation view for contributors. The normative contracts live in the design docs: [API Surfaces](/design/api-surfaces/) owns the surface ↔ transport map, and the [Module Map](/design/module-map/) maps every crate and module to its owning doc.

## API

We need a high-performance backend with a modern, developer-friendly interface for building rich client apps on all platforms.

To achieve this, Capsule employs a **hybrid API strategy** that balances performance, flexibility, and reliability. Here's a breakdown of the core technology decisions and why they were made.

### Requirements

- **Performance Optimization:** Each data channel should use the best-fit protocol.
- **Developer Experience:** UI and backend teams can move independently with tools tailored to their workflows.
- **Cross-Platform Consistency:** Various data models should be serializable and deserializable across platforms.
- **Network Efficiency:** Use binary formats where necessary to reduce payload size and energy use, especially on mobile.
- **Scalability:** Decoupled subsystems (sync, uploads, media serving, UI) with differing performance requirements and domains must be able to scale independently.

### Technology Stack

| Technology | Use Case | Benefits |
|------------|----------|----------|
| **gRPC + Protocol Buffers** | Bulk metadata sync, initial sync, delta updates, federation pull | - Substantially smaller payloads than JSON<br>- Efficient for syncing thousands of records<br>- Strongly typed to reduce data corruption |
| **REST + OpenAPI 3.1** | Request/response surfaces (auth, media, upload control); UI queries answered client-side over the synced `library.sqlite` | - Debuggable with plain HTTP tooling<br>- Generates the typed `capsule-sdk` REST client<br>- No server-readable content required (key-free model) |
| **HTTP + TUS-style resumable upload** | Uploading and downloading original photo assets | - Resume-capable uploads for poor networks<br>- CDN-compatible<br>- Built for large file transfers |
| **Offline-First Architecture** | Local caching, editing, and sync | - Guarantees smooth experience regardless of connectivity<br>- Local-first UX with background resolution and merge |

The upload protocol is modeled on [TUS](https://tus.io/) v1's offset/`PATCH` model rather than adopting it wholesale — see [Import — Upload Protocol](/design/import/upload-protocol/) for the exact header set and strictness rules.

### Some Technical Notes

- **No gRPC proxy is required.** `capsule-api-sync` mounts the `capsule.sync.v1` tonic service on the same Salvo router as the REST surfaces, and wraps it in `tonic_web::GrpcWebLayer` so the *same* service answers both native gRPC (`application/grpc`, over h2c) and browser gRPC-web calls. There is no Envoy or Istio sidecar in the deployment, and no separate metadata service: one binary serves every surface.
- **Object storage vs. file storage:** long-term blob storage is a plain filesystem tree under `UPLOAD_DIR` (see [Filesystem — Server](/design/filesystem/server/)), addressed by content hash. File storage gives high-throughput, low-latency block-level access, avoids network round-trips for large originals, and is far easier to reason about in a self-hosted deployment. There is no object-storage dependency in the server today.
- **Filesystem requirements:** the only hard requirement the server states is that the whole blob tree live on a **single** filesystem, so that the finalization `rename` into `blobs/{hash}.bin` is atomic — see [Filesystem — Server](/design/filesystem/server/) and [Filesystem — Maintenance](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery). Any POSIX filesystem meeting that holds; no particular filesystem is certified.

## Clients

We prefer native client applications where possible for consistent UX and leveraging platform-specific features.

Capsule has clients for the following platforms:

- [Android](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-android) — Jetpack Compose app over the `capsule-core-kotlin` shared library
- [iOS/macOS](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-swift) — SwiftUI app over the `capsule-core-swift` shared library
- [Web](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-web) — React client (guest drops and the share-link viewer; the authenticated read path is a key-free projection of the sync feed)
- [CLI](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-cli) — for development and advanced users primarily

The native apps and the CLI share their client logic through `capsule-core` and `capsule-sdk`, exposed to Swift and Kotlin as uniffi bindings. There is no desktop application: Windows and Linux users are served by the CLI and the web client. See [Clients](/design/clients/) for the per-platform contract.
