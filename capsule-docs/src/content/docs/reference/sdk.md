---
title: SDK
description: Which Capsule SDK surface each platform builds on, and how each page stays true
status: draft
---

Every Capsule client is built on one Rust crate, `capsule-sdk`. The native apps reach it through
generated bindings, and the read-only web client reaches the sealing code through a separate
WebAssembly module. This section has one page per language a client is written in:

| Page | Surface | Who uses it | Generated from | Kept current by |
| --- | --- | --- | --- | --- |
| [Rust](/reference/sdk/rust/) | the `capsule-sdk` crate | the CLI and TUI, and every binding below | rustdoc (see [Crates](/reference/crates/)) | `mise run doc-check-rust` |
| [Swift](/reference/sdk/swift/) | the `capsule_sdk` uniffi namespace | the iOS, iPadOS and macOS apps | `capsule-sdk/bindings-surface.json` | `mise run gen-bindings --check` |
| [Kotlin](/reference/sdk/kotlin/) | the `capsule_sdk` uniffi namespace | the Android app | `capsule-sdk/bindings-surface.json` | `mise run gen-bindings --check` |
| [Browser (WASM)](/reference/sdk/wasm/) | the `capsule-wasm` module | the web client's share viewer and guest uploader | `capsule-wasm/wasm-surface.d.ts` | `mise run wasm-surface-check` |

## One surface, three spellings

The Swift and Kotlin pages describe the same `capsule_sdk` namespace, generated library-mode from
one compiled cdylib by one pinned uniffi. They differ only in how each language spells it: a Rust
`seal_asset` becomes `sealAsset` in both, a Rust record becomes a Swift `struct` and a Kotlin
`data class`, and a Rust error enum becomes a Swift `Error` and a Kotlin exception. A verb present
on one page and missing from the other is a defect in the binding build, and the dump step fails
on it.

The browser module is not a binding of `capsule-sdk`. The web client is read-only, so
`capsule-wasm` exports only what opening a share link and sealing a guest drop need, compiled
from `capsule-core` for `wasm32`.

## How the generated pages stay true

The generated Swift, Kotlin and JavaScript sources are build output and are not committed, so the
documentation build cannot read them. Each generation step therefore writes a small committed
dump beside it:

- `mise run gen-bindings` generates the uniffi sources, checks that every verb the native clients
  consume crossed the namespace boundary, and writes each one's declaration and doc comment to
  `capsule-sdk/bindings-surface.json`. The Rust gate runs it with `--check`, so a changed
  signature fails CI until the dump is regenerated and committed.
- `mise run wasm-surface` copies the public part of the `capsule_wasm.d.ts` that wasm-bindgen
  emits to `capsule-wasm/wasm-surface.d.ts`. The web gate runs `wasm-surface-check`, because only
  the web job installs wasm-bindgen.

A generated page is never edited. If something on one is wrong, fix the Rust doc comment it came
from and regenerate. The contract is [Developer Documentation](/design/developer-docs/).
