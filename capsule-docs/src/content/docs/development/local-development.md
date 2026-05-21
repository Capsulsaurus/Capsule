---
title: Local Development
description: How to set up your local development environment for Capsule API.
---

While there is no one size-fits-all to setting up a development environment for all the packages in the Capsule API, there are some essential tools necessary to validate certain aspects during development. This page mentions tools developers should be aware of to incorporate into development, especially towards finalizing issues/PRs.

## How local development works?

### Requirements Summary

- Develop multiple Rust-based services with hot-reloading without rebuilding images
- Maintain a single source of configuration for both dev and production
- Consolidated logging in a single terminal window
- Support for Kubernetes-specific tests
- Lightweight resource footprint for development
- Support for service mesh communication patterns (using existing Istio)
- Routing through production-equivalent ingress (Envoy which is used internally by Istio)

### Technology Choices: Key Motivations

#### K3d

K3d was selected for its extremely lightweight Docker-based Kubernetes implementation, offering significantly faster startup times and lower resource consumption compared to alternatives like Minikube.

#### Skaffold

Skaffold provides file synchronization without image rebuilding, integrates directly with existing Helm charts, and offers consolidated logging from all services in a single terminal while maintaining a single source of truth for configurations.

#### Cargo-watch

Essential for Rust-specific hot-reload capabilities, cargo-watch monitors file changes within containers to trigger recompilation without rebuilding container images, creating a true hot-reload experience for compiled Rust services.

#### Stern (Optional)

Provides enhanced log filtering capabilities when dealing with complex interactions between multiple services, complementing Skaffold's basic consolidated logging.
