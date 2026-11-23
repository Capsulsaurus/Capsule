---
title: Component Map
description: How the components of the Capsule monorepo depend on each other, from the shared Rust core out to each app and the server
audit: none
---

Capsule is one repository. A Rust core holds the logic every client shares, each app reaches it
through a binding for its platform, and the server is a separate Rust binary that speaks to clients
only over its REST contract. What crosses that contract is the subject of
[](/development/architecture/).

## Dependencies

```mermaid title="Each app links the shared Rust core through its platform's binding; the server meets clients only over REST"
flowchart TB
  swift[capsule-swift]:::client
  android[capsule-android]:::client
  web[capsule-web]:::client
  cli[capsule-cli]:::client
  ffi[capsule-core-ffi]:::accent
  kotlin[capsule-core-kotlin]:::accent
  wasm[capsule-wasm]:::accent
  sdk[capsule-sdk]
  library[capsule-library]
  ptpip[ptpip-rs]
  core[capsule-core]
  rawshift[Rawshift]:::external
  chromahash[Chromahash]:::external
  server[capsule-server]:::server
  pg[(Postgres)]:::store
  blobs[(Blob store)]:::store

  swift --> ffi
  android --> kotlin
  web --> wasm
  cli --> sdk
  ffi --> sdk
  kotlin --> sdk
  wasm --> core
  sdk --> library
  library --> core
  library --> ptpip
  core --> rawshift
  core --> chromahash
  sdk -- ciphertext over REST --> server
  server --> core
  server --> pg
  server --> blobs
```

An arrow points from a component to what it depends on, through the path that carries the
architecture: each app reaches the shared Rust through a binding for its platform (the accent
nodes), the CLI directly, and the server is a separate binary the SDK reaches only over REST. Shorter links that skip a step
(the CLI and the Apple binding also linking `capsule-library` directly, and the SDK, the CLI, the server and
`xtask` using `capsule-i18n`) are left out. One link is a build step rather than a dependency: Spargen generates
the SDK's REST client from the OpenAPI document the server emits, so the client cannot drift from
the contract.

## Components

| Component | Role |
| --- | --- |
| [`capsule-core`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-core) | Stateless shared logic: keys and encryption, media policy, sidecars and provenance. It holds no state and does no I/O of its own. |
| [`capsule-library`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-library) | A client's local state over the core: the SQLite catalog, the on-disk library and its lock, import scanning and backup file I/O. |
| [`capsule-sdk`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-sdk) | The generated REST client wrapped in Capsule's workflows, and the app-facing uniffi namespace (`capsule_sdk`) through which the apps reach the library and core. |
| [`capsule-i18n`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-i18n) | Runtime localization for the Rust surfaces, from the strings in the repository's `locales/`. |
| [`ptpip-rs`](https://github.com/Capsulsaurus/Capsule/tree/master/ptpip-rs) | A pure-Rust PTP/IP initiator for importing straight from a camera ([](/design/import/camera-import/)). |
| [`capsule-core-ffi`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-core-ffi) | The one static library an Apple app links, bundling the SDK's and library's uniffi surface. |
| [`capsule-core-kotlin`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-core-kotlin) | The Android library over the SDK's shared object and its generated Kotlin bindings. |
| [`capsule-core-swift`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-core-swift) | A SwiftPM harness proving the Swift bindings work before the app links them. |
| [`capsule-wasm`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-wasm) | The browser surface over the core: opening share links and sealing guest uploads client-side. |
| [`capsule-swift`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-swift) | The SwiftUI app for iPhone, iPad and Mac. |
| [`capsule-android`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-android) | The Jetpack Compose app. |
| [`capsule-web`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-web) | The web client: authenticated viewing, share links and guest upload. |
| [`capsule-cli`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-cli) | The `capsule` binary and the reference client: every user flow lands here first and is reachable as a `--json` subcommand. |
| [`capsule-server`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-server) | The Kynos REST/OpenAPI server, holding ciphertext and a key-free index ([](/development/architecture/)). |
| [`capsule-e2e`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-e2e) | End-to-end cases over the real SDK, library and server, wired together the way they ship ([](/development/testing/)). |
| [`capsule-docs`](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-docs) | This site, including the reference pages generated from the server's and SDK's surfaces. |
| [`xtask`](https://github.com/Capsulsaurus/Capsule/tree/master/xtask) | Repository maintenance tasks. |

Each app's platform contract is [](/design/clients/), and the rules for what may become a shared
library are under [](/development/architecture/#dependency-boundaries).
