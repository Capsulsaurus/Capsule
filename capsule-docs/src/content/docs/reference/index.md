---
title: Reference
description: Generated reference for Capsule's developer surfaces
status: draft
---

Each surface's reference is generated from a committed, machine-readable artifact its own
toolchain emits and its own gate keeps current; the docs build only reads those files. Hand-written
prose is limited to each section's overview, so a wrong page is fixed at the source annotation.
The pipeline is [](/design/developer-docs/).

| Surface | Overview | Generated from | Kept current by |
| --- | --- | --- | --- |
| REST | [](/reference/api/) | `capsule-server/openapi.json` | `mise run openapi-check-kynos` |
| Command line | [](/reference/cli/) | `capsule-cli/cli-surface.json` | `mise run cli-surface-check` |
| Swift and Kotlin | [](/reference/sdk/) | `capsule-sdk/bindings-surface.json` | `mise run gen-bindings --check` |
| Browser | [](/reference/sdk/) | `capsule-wasm/wasm-surface.d.ts` | `mise run wasm-surface-check` |
| Rust crates | [rustdoc on GitHub Pages](https://capsulsaurus.github.io/Capsule/) | the workspace's doc comments, rendered from master | `mise run doc-check-rust` |

The [](/reference/sdk/rust/) SDK page is hand-written: it maps the crate a client builds on, and
rustdoc holds its item-level reference.
