---
title: Reference
description: Generated reference for Capsule's developer surfaces
status: draft
---

Reference for every Capsule developer surface. Each section is generated from a **description
artifact**: a small, committed, machine-readable file that the surface's own toolchain emits and
its own gate keeps current. The documentation build reads those files and never invokes cargo,
uniffi, or wasm-bindgen — which is what keeps a reference page from disagreeing with the code it
describes. [Developer Documentation](/design/developer-docs/) is the contract; this page is the
index to it.

Reference pages are generated, never written. Every section's hand-written prose is confined to
its overview page — what the surface is for, how to reach it, where its contract lives. If a
generated page is wrong, the annotation in the source is wrong.

## Published

| Surface | Overview | Generated from | Kept current by |
| --- | --- | --- | --- |
| REST | [REST API](/reference/api/) | `capsule-server/openapi.json` | `mise run openapi-check-kynos` |
| Command line | [CLI](/reference/cli/) | `capsule-cli/cli-surface.json` | `mise run cli-surface-check` |

## Not published yet

These surfaces are named here rather than given an empty route, because a dead link is worse
than an honest absence.

- **Rust SDK** and **workspace rustdoc.** The workspace is `publish = false`, so docs.rs will
  never build it; rustdoc is built by the Rust gate and deployed beside this site rather than
  committed. Planned as `/reference/sdk/rust/` and `/reference/crates/`.
- **Swift and Kotlin bindings.** The generated bindings are gitignored build output, so the
  bun-only documentation build cannot read them; each needs a committed surface dump alongside
  its existing generation step first.
- **Browser surface.** The same problem for `capsule_wasm.d.ts`, with the additional constraint
  that its drift gate cannot run where the other Rust gates do.

Until then:

- The REST surface-to-transport map is [API Surfaces](/design/api-surfaces/), and the contract
  rules server code follows are [API Practices](/development/api-practices/).
- Code module to owning design doc is the [Module Map](/design/module-map/).
