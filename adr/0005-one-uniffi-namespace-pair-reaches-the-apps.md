# ADR-0005 — One uniffi namespace pair reaches the apps

- **Status:** accepted (amended 2026-10-18)
- **Date:** 2026-09-01
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Module Map — Client Boundaries](../capsule-docs/src/content/docs/design/module-map.md#client-boundaries)
- **Slices:** S-F1, S-F3, S-D9, S-P1

The decision below stands. The third namespace its Context and Consequences describe no
longer exists; the [amendment](#amendment-2026-10-18) is what the record now says about it.

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

## Amendment (2026-10-18)

The `capsule_core` namespace is deleted (#424, #579). By the time it went, nothing an app
links used it: its only consumers were the `capsule-core-kotlin` and `capsule-core-swift`
hardware harnesses, and every surface they drove — the Ed25519 and P-256 hardware-signer
constructors, the key-agreement seam, the hardware DEK round trip, seal, verify and read —
had been authored in `capsule_sdk` first. The harnesses now implement `FfiHardwareSigner`
and `FfiHardwareKeyAgreement` from `capsule_sdk` and link `libcapsule_sdk`, and
`capsule-core` carries no `ffi` feature, no uniffi dependency and no `cdylib`/`staticlib`
crate type. Its hardware seams are plain Rust traits that `capsule_sdk` adapts, one call per
method.

1. **Two namespaces in the workspace, not three.** `capsule_core_ffi` and `capsule_sdk` are
   the whole of what exists, and they are the pair an app links. The `S-F1` invariant that
   `capsule_core` never shares a binary with `capsule_sdk` has no subject left; what replaces
   it is that `capsule-core` exports no namespace, so no crate an app links can contribute
   scaffolding of its own. `mise-tasks/gen-bindings` generates `capsule_sdk` alone and fails
   if its cdylib yields any other namespace.
2. **The mobile cross-builds build `capsule-sdk --features ffi`.** `build-android`,
   `build-linux-cross` and `build-windows` build it in place of `capsule-core --features
   ffi`, and `build-apple` builds it beside the `capsule-core-ffi` umbrella. It is the
   library the harnesses load — the Android `.so` comes from its `cdylib` — and it links
   `capsule-core`, so the targets still prove `capsule-core` cross-compiles.
   `capsule-core-ffi` was the other candidate and was not taken for Android: it is a
   `staticlib` only, so it yields no `.so` for the Kotlin bindings to load.
3. **The consequence that named the third namespace as a cost is discharged.** There is no
   crypto surface left that is reachable only through what `capsule_core_ffi` re-exposes; a
   verb an app needs is added to `capsule_core_ffi` or `capsule_sdk`, as the first
   consequence already says.
