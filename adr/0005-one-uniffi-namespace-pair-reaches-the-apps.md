# ADR-0005 — One uniffi namespace pair reaches the apps

- **Status:** accepted
- **Date:** 2026-09-01
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Module Map — Client Boundaries](../capsule-docs/src/content/docs/design/module-map.md#client-boundaries)
- **Slices:** S-F1, S-F3, S-D9, S-P1

## Context

Three uniffi namespaces exist in this workspace, and the number is easy to misread as
three things an app links.

- `capsule_core`, behind `capsule-core`'s `ffi` feature: the crypto `FfiWorkspace` and the
  `HardwareSigner` foreign trait.
- `capsule_core_ffi`, in its own crate: the SQLite catalog, the CBOR sidecar, the
  `GatedView`/`LocalAuthGate` seam and the single `CatalogError` that crosses the boundary.
- `capsule_sdk`, behind `capsule-sdk`'s `ffi` feature: the networked user flows (`S-D9`)
  and the workspace verbs the iOS lane consumes (`S-P1`).

`S-F1` settled that `capsule_core` and `capsule_core_ffi` are *layered* — distinct crates,
distinct bindings namespaces, one pinned uniffi version — and that they are **never linked
into the same binary**, so their generated scaffolding cannot collide.

What an app actually links follows from a property of the toolchain rather than from a
preference: two Rust staticlibs cannot share a binary, because each bundles its own `std`.
So any namespace an app needs must ride in one library. `capsule-core-ffi` is that library
(`S-F3`): it is the app umbrella staticlib, and it carries `use capsule_sdk as _;` for the
sole purpose of forcing rustc to keep the SDK's scaffolding and metadata in the archive,
since nothing in the crate calls it — the generated Swift does, over the C ABI.
`capsule-sdk` deliberately does not enable `capsule-core/ffi`, which is what keeps the
`S-F1` never-same-binary invariant intact.

The Apple graph shows the result: `CapsuleCatalogFFI` compiles
`.ffi/generated/capsule_core_ffi.swift` and `.ffi/generated/capsule_sdk.swift`, and no
other generated namespace.

## Decision

An app links exactly the **`capsule_core_ffi` + `capsule_sdk`** namespace pair, delivered
as one staticlib. `capsule_core` is not an app-facing namespace: it is the crypto surface
`capsule-core-ffi` and the harnesses use, and it never shares a binary with `capsule_sdk`.

## Consequences

- A new verb an app needs is added to `capsule_core_ffi` or to `capsule_sdk`. Adding a
  third namespace to the app's link line is not an option the toolchain leaves open.
- `mise-tasks/gen-bindings` keeps generating `capsule_core` and `capsule_sdk` separately,
  from their own compiled cdylibs, and its symbol-presence assertions keep proving that
  each surface crossed. Generating them together would violate `S-F1`.
- `capsule-core-ffi`'s `use capsule_sdk as _;` is load-bearing and must not be removed as
  an unused import. Without it the linker drops the `capsule_sdk` scaffolding and every
  networked verb disappears from the app with no compile error.
- The third namespace is a cost this decision names rather than hides: `capsule_core`'s
  crypto surface is reachable only through whatever `capsule_core_ffi` re-exposes. #399
  is where that surface is frozen and the dead parts of it removed.

## Considered and rejected

- **One namespace for everything.** It would put the crypto `FfiWorkspace` and the
  networked flows in one uniffi surface, which is the collision `S-F1` exists to prevent
  and which would make `capsule-core`'s crypto tree an app dependency.
- **Two staticlibs, one per namespace.** Not available: each bundles its own `std`, so the
  app would carry two runtimes and the symbols would clash.
