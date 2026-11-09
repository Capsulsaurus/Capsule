---
paths:
  - "capsule-core-ffi/**"
---

# capsule-core-ffi

## Rules

- This crate is the Apple app's umbrella staticlib: it carries the `capsule_core_ffi` catalog namespace and links `capsule-sdk`'s `capsule_sdk` namespace, because two Rust staticlibs cannot share one binary. Any namespace the app needs must ride in it.
- Its Swift bindings come from capsule-swift's FFI build (`mise run build-ffi-apple`, which runs `capsule-swift/Scripts/build-rust-ffi.sh`), not from `mise run gen-bindings`, which generates only the `capsule_sdk` namespace.

## Docs

Follow:

- [Local gallery](../../capsule-docs/src/content/docs/design/local-gallery.md) — the on-device gallery this surface exposes.
- The crate docs in [`src/lib.rs`](../../capsule-core-ffi/src/lib.rs) and [ADR-0005](../../adr/0005-one-uniffi-namespace-pair-reaches-the-apps.md) — the client boundary and the namespace pair.
- [Architecture](../../capsule-docs/src/content/docs/development/architecture.md) — data flow into the native clients.

Keep current:

- [SDK reference](../../capsule-docs/src/content/docs/reference/sdk.md) — when the exported surface changes.
