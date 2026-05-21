---
title: Self-Hosting
description: Get control of your data
---

Capsule was meant to be self-hosted from the start. However, with its emphasis on performance and robustness, it has several components that require careful setup. This guide will explain everything inside the `capsule-api` package which consist of sub-components for various functionality.

Skip to [Deploying](#deploying) to see quick start instructions.

## Prerequisites

Unlike other open-source projects, Capsule makes some assumptions about your environment and provides very specific instructions for setup. Assume some technical knowledge and while some technical details require some quick googling or "GPTing", the one-click installer should be sufficient for most users.

## Hardware

*For single-node deployment. For Kubernetes, you may adjust to your requirements.*

- Operating System: Some modern GNU/Linux distribution capable of running containers (Docker or some Kubernetes distribution)
- CPU: Most modern x86 and arm64 chips should work. AES-NI should be enabled. Intel QAT is not necessary for TLS (`rustls`).
- RAM:
  - Minimum: 2 GB
  - Recommended: 4 GB
- Storage: (minimum requirements TBD)

## Software

Since Capsule extensively uses container technologies for both development and production, the specific software requirements are less important other than to ensure potential compatibility issues are isolated for the most common, popular target (i.e., some sort of Linux distribution with glibc). For beginners, we recommend installing the newest Ubuntu LTS server (although other distributions like Rocky Linux are officially tested on). Docker/Podman installation method would be easiest too unless you have multiple-nodes or look for service resiliency.

### Components

The Capsule API is written almost entirely in Rust with several binary components serving distinct purposes:

- [GraphQL](/capsule-api/graphql/): GraphQL API for majority of user-facing functionality. Flexible and cross-platform.
- [Upload](/capsule-api/upload/): A performant TUS-based upload service. Enables high-throughput, resumable uploads.
- [Metadata](/capsule-api/metadata/): Used for efficient metadata fetching and updating. Consists of two parts:
  - A gRPC (web) service for efficient fetching and updating metadata. We strictly prefer binary-based protocols (i.e. no JSON) for lower-serialization costs with mobile clients.
  - WebSocket + ProtoBuf service for efficient real-time updates
<!-- TODO: this section is outdated ^^ -->

*Note: These components may be combined into a single web server for low-resource environments. It is used in the one-click Docker installer as well.*

External dependencies:

- [PostgresSQL](https://www.postgresql.org/): Main datastore for application data
- [Valkey](https://valkey.io/): Key-value store for caching and ephemeral data
- [MinIO](https://min.io/): Object storage
- *Various filesystems*: Various API components (e.g. `capsule-upload` and `capsule-metadata`) need direct filesystem access with various requirements (explained below).

## Deploying

### One-Click Docker Installer

Prior to installation, double check you have planned for the following requirements:

- Sufficient flash storage (e.g. 16 GB+) for temporary chunked writes for `capsule-upload`

<!-- TODO -->

### Kubernetes

*Disclaimer: This is the most robust and thoroughly-tested method. However, despite all the explicit documentation to avoid potential issues, it requires technical knowledge with Kubernetes.*

Capsule tests on vanilla Kubernetes installations. However, it is known to work on most standards-compliant Kubernetes distributions. If uncertain, we recommend using RKE2 with Cilium/Calico just because the developer uses it personally.

<!-- TODO -->
