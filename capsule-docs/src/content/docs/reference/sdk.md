---
title: SDK
description: Which Capsule SDK surface each platform builds on
status: draft
---

Every Capsule client is built on one Rust crate, `capsule-sdk`. The native apps reach it through
generated bindings, and the read-only web client reaches the sealing code through a separate
WebAssembly module. This section has one page per language a client is written in; each is
generated from the code and verified in CI.

| Page | Surface | Who uses it |
| --- | --- | --- |
| [](/reference/sdk/rust/) | the `capsule-sdk` crate ([rustdoc](/rustdoc/capsule_sdk/)) | the CLI and TUI, and every binding below |
| [](/reference/sdk/swift/) | the `capsule_sdk` uniffi namespace | the iOS, iPadOS and macOS apps |
| [](/reference/sdk/kotlin/) | the `capsule_sdk` uniffi namespace | the Android app |
| [](/reference/sdk/wasm/) | the `capsule-wasm` module | the web client's share viewer and guest uploader |

## One surface, three spellings

The Swift and Kotlin pages describe the same `capsule_sdk` namespace, generated library-mode from
one compiled cdylib by one pinned uniffi. They differ only in spelling.

| Rust | Swift | Kotlin |
| --- | --- | --- |
| `seal_asset` | `sealAsset` | `sealAsset` |
| Record | `struct` | `data class` |
| Error enum | `Error` | Exception |

A verb present on one page and missing from the other is a defect in the binding build, and the
dump step fails on it.

The browser module is not a binding of `capsule-sdk`. The web client is read-only, so
`capsule-wasm` exports only what opening a share link and sealing a guest drop need, compiled
from `capsule-core` for `wasm32`.
