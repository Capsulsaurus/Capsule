---
title: Architecture
description: How Capsule combines gRPC, GraphQL, TUS, and more to deliver a high-performance and robust API.
---

Capsule is a cross-platform photo service designed for professional and enthusiast photographers who expect fast syncing, seamless uploads, and powerful search—regardless of device or network conditions.

## API

We need a high-performance backend with a modern, developer-friendly interface for building rich client apps on all platforms.

To achieve this, Capsule employs a **hybrid API strategy** that balances performance, flexibility, and reliability. Here's a breakdown of the core technology decisions and why they were made.

### Requirements

- **Performance Optimization:** Each data channel should use the best-fit protocol:
- **Developer Experience:** UI and backend teams can move independently with tools tailored to their workflows.
- **Cross-Platform Consistency:** Various data models should be serializable and deserializable across platforms.
- **Network Efficiency:** Use binary formats where necessary to reduce payload size and energy use, especially on mobile.
- **Scalability:** Decoupled subsystems (sync, search, uploads, UI) with differing performance requirements and domains must be able to scale independently.

### Technology Stack

| Technology | Use Case | Benefits |
|------------|----------|----------|
| **gRPC + Protocol Buffers** | Bulk metadata sync, initial sync, delta updates | - 60–80% smaller payloads than JSON<br>- Highly efficient for syncing thousands of records<br>- Strongly typed to reduce data corruption |
| **GraphQL** | UI queries, search, and filtering | - Ideal for flexible UI data needs<br>- Reduces API iteration cycles<br>- Great developer experience for frontend teams |
| **HTTP + TUS Protocol** | Uploading and downloading original photo assets | - Resume-capable uploads for poor networks<br>- CDN-compatible<br>- Built for large file transfers |
| **WebSockets + Protocol Buffers** | Real-time sync status, presence, notifications | - Efficient binary messaging<br>- Reuses existing protobuf models<br>- Low-latency delivery for system-level events |
| **GraphQL Subscriptions** | User-facing real-time events (comments, shares, collab) | - Easy to use in UI clients<br>- Strong typing<br>- Filtering and selective subscriptions |
| **Offline-First Architecture** | Local caching, editing, and sync | - Guarantees smooth experience regardless of connectivity<br>- Local-first UX with background resolution and merge |

### Some Technical Notes

- For gRPC and gRPC-Web traffic, we use Envoy as proxy (or via Istio for Kubernetes deployments) because at the time of creation, it is the most mature and robust solution. As such, for Kubernetes deployments, Envoy needs to be installed as a sidecar on every pod hosting Capsule services using gRPC (e.g. capsule-metadata​)
- Object storage vs. File storage: While we use object storage for ephermal storage, we use file storage for long-term storage. The use of object storage for the former needs no explanation (standard industry practice). However, for the latter, we use file storage for high-throughput and low-latency block-level access. For most self-hosted deployments, filesystems are also easier to manage and maintain. By storing (potentially large) files on file storage, we could save network bandwidth on back-and-forth copies and have faster block-level access essential to metadata and thumbnail generation.
- Filesystems: While Kubernetes abstracts filesystem features behind POSIX standards. For consistency, we officially target and support ZFS and XFS via NFS and Ceph RBD

## Clients

We prefer native client applications where possible for consistent UX and leveraging platform-specific features.

Capsule has several native clients for the following platforms:

- [Android](https://github.com/justin13888/Capsule/tree/master/capsule-android)
- [iOS/macOS](https://github.com/justin13888/Capsule/tree/master/capsule-swift)
- [Desktop](https://github.com/justin13888/Capsule/tree/master/capsule-desktop) (supports Windows and Linux)
- [Web](https://github.com/justin13888/Capsule/tree/master/capsule-web)
- [CLI](https://github.com/justin13888/Capsule/tree/master/capsule-cli) (for development and advanced users primarily)
