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

## Not published yet

- **Rust SDK and workspace rustdoc** (the workspace is `publish = false`, so not on docs.rs).
- **Swift and Kotlin bindings** and the **browser surface** (`capsule_wasm.d.ts`): generated
  output is not committed, so each needs a committed surface dump first.

Until then:

- The REST surface-to-transport map is [API Surfaces](/design/api-surfaces/), and the contract
  rules server code follows are [API Practices](/development/api-practices/).
- Code module to owning design doc is the [Module Map](/design/module-map/).
