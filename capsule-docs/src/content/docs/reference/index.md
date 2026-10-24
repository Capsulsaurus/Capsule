---
title: Reference
description: Generated reference for Capsule's developer surfaces
status: draft
---

Reference for Capsule's developer surfaces. Each section is generated from a committed,
machine-readable **description artifact** that the surface's own toolchain emits and its own gate
keeps current; the docs build only reads those files. Hand-written prose is limited to each
section's overview page. If a generated page is wrong, fix the source annotation. The contract is
[Developer Documentation](/design/developer-docs/).

## Published

| Surface | Overview | Generated from | Kept current by |
| --- | --- | --- | --- |
| REST | [REST API](/reference/api/) | `capsule-server/openapi.json` | `mise run openapi-check-kynos` |
| Command line | [CLI](/reference/cli/) | `capsule-cli/cli-surface.json` | `mise run cli-surface-check` |
| Swift and Kotlin | [SDK](/reference/sdk/) | `capsule-sdk/bindings-surface.json` | `mise run gen-bindings --check` |
| Browser | [SDK](/reference/sdk/) | `capsule-wasm/wasm-surface.d.ts` | `mise run wasm-surface-check` |
| Rust crates | [Crates](/reference/crates/) | workspace rustdoc, not committed | `mise run doc-check-rust` |

The [Rust](/reference/sdk/rust/) SDK page and [Crates](/reference/crates/) are hand-written: the
Rust surface's item-level reference is rustdoc, which is built by the Rust gate and is too large
to commit.

Related:

- The REST surface-to-transport map is [API Surfaces](/design/api-surfaces/), and the contract
  rules server code follows are [API Practices](/development/api-practices/).
- Code module to owning design doc is the [Module Map](/design/module-map/).
