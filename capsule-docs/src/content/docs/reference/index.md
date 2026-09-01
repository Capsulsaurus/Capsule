---
title: Reference
description: Generated reference for Capsule's developer surfaces
status: draft
---

This section will hold generated reference for every Capsule developer surface — the REST contract,
the command line, the Rust SDK, the Swift and Kotlin bindings, and the browser surface.

**Nothing is published here yet.** The pipeline that emits these pages is specified in
[Developer Documentation](/design/developer-docs/), which names each surface, the artifact it is
generated from, and the gate that proves that artifact current. Until a surface's emitter and drift
gate exist, its page is deliberately absent rather than hand-written and stale.

In the meantime:

- The REST surface-to-transport map is [API Surfaces](/design/api-surfaces/), and the contract rules
  server code follows are [API Practices](/development/api-practices/).
- Code module to owning design doc is the [Module Map](/design/module-map/).
- `capsule --help` is the current source of truth for the command line.
