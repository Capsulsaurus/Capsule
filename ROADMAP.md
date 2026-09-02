# Roadmap

The **package-level** view of Capsule: one row for every package the repository declares, in
every toolchain, with the state it is actually in today.

[`SLICES.md`](SLICES.md) stays the slice-level tracker and is the authority on any slice's
status. This file never restates one. The `Open slices` column cites ids so a reader can get
from a package to the work outstanding against it; the status of that work is read there, not
here. A slice appears against the package whose tree its deliverable lands in, so a slice that
spans a client and a server leg is listed against both halves only where both are genuinely
owed.

`mise run check-docs-truth` runs a `roadmap` check over this file. It resolves every row
against the manifests themselves — `Cargo.toml`, `settings.gradle.kts`,
`capsule-swift/Project.swift`, the `Package.swift`/`package.json`/`pyproject.toml` package
roots, `locales/`, `.gitmodules`, and `legacy-review/*/` — so adding a package to the tree
fails the gate until this file gains a row for it. Every `Gate` cell must name a real `mise`
task and every cited slice id must have a detail block in `SLICES.md`.

## States

A closed set. A row's state is a claim about the package, not about the programme.

- `frozen` — ships, contract settled, no open slices. Only defect fixes land.
- `stabilizing` — live and inside a `mise run check-*` gate, with the contract still moving.
  Open slices refine it.
- `rebuilding` — the shipped surface is quarantined under `legacy-review/` and is being
  re-landed on a replacement.
- `blocked` — cannot start. A named dependency outside this package gates it.
- `deferred` — in scope and deliberately unscheduled. Post-v1.
- `review-only` — non-buildable reference material. No gate, no build.
- `excluded` — in the tree but outside the shipped build: a submodule, a conditionally
  compiled target, or research material. Any gate such a row names is format and lint only,
  and the `Notes` cell says so.

## Packages

| Package | Kind | Owns | State | Gate | Owner docs | Open slices | Next milestone | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `capsule-core` | cargo | The offline crypto data plane, catalog, signed sidecars, import pipeline, LQIP, and the OpenMLS authority | stabilizing | `mise run check-rust` | [Module Map](capsule-docs/src/content/docs/design/module-map.md) | `S-B1`, `S-B5`, `S-B13`, `S-D24`, `S-D29` | Public-API freeze (#399) | `capsule-core::media` is designed and unbuilt, so there is no image decoder in the workspace and every still import is a `DeferredNoCodec` |
| `capsule-core-ffi` | cargo | The app umbrella staticlib and the `capsule_core_ffi` uniffi namespace | stabilizing | `mise run check-rust` | [Module Map — Client Boundaries](capsule-docs/src/content/docs/design/module-map.md#client-boundaries) | — | Public-API freeze (#399) | Links `capsule-sdk`'s uniffi surface so one Rust library carries both namespaces an app consumes |
| `capsule-sdk` | cargo | Session, upload, sync, recovery and protocol-version orchestration over the spargen-generated REST client | stabilizing | `mise run check-rust` | [API Surfaces](capsule-docs/src/content/docs/design/api-surfaces.md) | `S-D1`, `S-D2`, `S-D7`, `S-D8`, `S-D9`, `S-D10`, `S-D17`, `S-E3`, `S-N2` | Re-front the gRPC sync half on REST (#408) | Replacement-in-progress, not review material: the wire contract is re-sourced from Kynos, the crate is not thrown away |
| `capsule-server` | cargo | The Kynos REST/OpenAPI application and the committed `capsule-server/openapi.json` contract | rebuilding | `mise run check-rust` | [Module Map — Server Modules](capsule-docs/src/content/docs/design/module-map.md#server-modules) | `S-C8`, `S-C39`, `S-C47`, `S-C49`, `S-C51`, `S-E2`, `S-E5`, `S-N1` | A binary, configuration and a serve task (#401) | Fifty-nine operations and a test suite over the real router, with no binary, no configuration loading and no Postgres or Valkey adapter |
| `capsule-wire` | cargo | Framework-free protocol headers and the response taxonomy across the retiring Salvo boundary | stabilizing | `mise run check-rust` | [API Surfaces](capsule-docs/src/content/docs/design/api-surfaces.md) | `S-C27` | Retired (#400) | Only retired code still depends on it; `capsule-server` owns `problem`, `limits` and `body` |
| `capsule-wasm` | cargo | The browser boundary — share-link open, guest drop sealing, and LQIP decode | stabilizing | `mise run check-rust` | [Web Upload](capsule-docs/src/content/docs/design/web-upload.md) | — | Public-API freeze (#399) | `S-B14` owes it an `lqip` entry point; the encoder already compiles for `wasm32-unknown-unknown` |
| `capsule-i18n` | cargo | The generated Rust catalog bundle, the runtime formatter, and the `error.*` code contract | stabilizing | `mise run check-rust` | [i18n](capsule-docs/src/content/docs/design/i18n.md) | — | ICU plural evaluation (#414) | Generated from `locales/` by `mise run i18n`; `mise run i18n-check` fails on drift |
| `capsule-cli` | cargo | The `capsule` binary — local library commands plus auth, sync, push, import and cull | stabilizing | `mise run check-rust` | [Clients](capsule-docs/src/content/docs/design/clients.md) | `S-B17`, `S-B18`, `S-I8`, `S-Q1`, `S-Q2`, `S-Q3`, `S-Q4` | Help text from the catalogs and an enrichment read surface (#413) | The networked commands have no server to reach until #401 lands one |
| `capsule-cli/entity` | cargo | sea-orm entities for the CLI's sync store — `sync_cursor` and `synced_asset` | stabilizing | `mise run check-rust` | [Clients](capsule-docs/src/content/docs/design/clients.md) | — | Follows `capsule-cli` (#413) | The one place `chrono` is permitted, as the sea-orm column type; convert at the entity boundary |
| `capsule-cli/migration` | cargo | sea-orm migrations for that store | stabilizing | `mise run check-rust` | [migration/README](capsule-cli/migration/README.md) | — | Follows `capsule-cli` (#413) | Schema changes land here before the entity crate sees them |
| `xtask` | cargo | Repository automation — `architecture-check`, `i18n-guard`, `translate-readme`, licence and workspace-dependency checks | stabilizing | `mise run check-rust` | [Developer Docs](capsule-docs/src/content/docs/design/developer-docs.md) | — | Guard-detector repair (#394, #414) | Not a shipped artifact; it is what makes several gates in `mise.toml` real |
| `capsule-android` | gradle | The Android application — Compose UI over the Kotlin core | blocked | `mise run check-kotlin` | [Clients](capsule-docs/src/content/docs/design/clients.md) | — | Make the build green (#389) | The app references a DI layer that is not in the tree, so it does not compile |
| `capsule-core-kotlin` | gradle | The Kotlin hardware-signer adapters — software P-256, software Ed25519, StrongBox | stabilizing | `mise run check-kotlin` | [Clients](capsule-docs/src/content/docs/design/clients.md) | — | StrongBox run on a device runner | Smoke-tested only; the device lane that would exercise StrongBox is unprovisioned |
| `capsule-core-swift` | swiftpm | The Swift hardware-signer adapters — Secure Enclave signing and key agreement, plus software fallbacks | stabilizing | `mise run check-swift` | [capsule-core-swift/README](capsule-core-swift/README.md) | `S-P6` | Secure-Enclave wiring into the app (`S-P6`) | `check-swift` formats and lints this package; `mise run test-swift` drives the Tuist workspace only, so its `swift test` suite is in no gate |
| `CapsuleFoundation` | tuist | Value types, logging and utilities for the Apple client. No dependencies | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Holds as the client's floor | The root of the Apple module graph; every other target depends on it |
| `CapsuleDomain` | tuist | The display and domain value types, as structural mirrors of the Rust records | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Mirrors hold across the FFI swap (`S-U19`) | Deliberately FFI-free so the mocked graph builds with no Rust toolchain |
| `CapsulePorts` | tuist | The protocol seams the app is written against, and which the mock and FFI adapters satisfy | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Move the six `FeatureAuth` ports here (#391) | Six ports are declared inside `FeatureAuth` today, which is what #391 corrects |
| `CapsuleNavigation` | tuist | `Route`, the sidebar catalog, deep-link classification and `ViewerContext` | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Every live sidebar row reaches a real screen | `SidebarItem.memories` and `.duplicates` are live rows whose screens are still scaffolds |
| `CapsuleMock` | tuist | The in-memory doubles the whole client is built and tested against | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Reachable scenario selection (#392) | `MockScenarioSelection` is write-only, so about thirty screens have no way in |
| `CapsuleDiagnostics` | tuist | The diagnostics coordinator and the client's own health surfaces | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Holds as the client's floor | — |
| `CapsuleCatalog` | tuist | The FFI-free catalog surface the app reads, and the error type that crosses it | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-P5` | Sync-apply renders into the local catalog (`S-P5`) | Written so the generated-type half can be swapped in without any screen naming a generated type |
| `CapsuleCatalogFFI` | tuist | The Rust-backed half — the generated `capsule_core_ffi` and `capsule_sdk` glue, record conversions and error mapping | excluded | — | [capsule-swift/README](capsule-swift/README.md) | `S-P8` | A behavioural FFI harness that flips `S-D9` | Present only under `TUIST_FFI=1`, so the default graph builds from a clean checkout with no cross-compile; format and lint reach it only when it is generated |
| `ManagedStore` | tuist | The Swift filesystem layer, hashing and the managed-store import pipeline | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Imports route through `ImportPort` (#390) | The picker importer writes this store directly, so picker imports never reach the timeline |
| `AssetKit` | tuist | Asset windowing, prefetch and the store the grid and viewer both read | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Holds as the client's floor | — |
| `CapsuleTestSupport` | tuist | Shared mocks and helpers for every module's unit tests | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Covers the suites still owed by lane U | Test-only; no product code depends on it |
| `ImagePipeline` | tuist | Decode, downsample and cache for the Apple client's image rendering | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Holds as the client's floor | — |
| `CapsuleUI` | tuist | The Capsule-state design system — the shared components every feature composes | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Accessibility inside the gate (#393) | The accessibility audit fails on most surfaces and is outside `check-swift` |
| `FeatureTimeline` | tuist | Library, timeline, selection and culling | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Culling review (`S-U7` remainder) | The uniform grid, pinch zoom and the zoom transition landed; culling review is outstanding |
| `FeatureViewer` | tuist | The viewer and asset detail | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Provenance and verdict detail (`S-U8` remainder) | The info panel, caption editing and the `.viewer` route landed |
| `FeatureAlbums` | tuist | Album index and detail, and the smart-album builder | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-U9` | The smart-album screen and predicate builder | Index and detail landed over the mock ports; members and policy editors are outstanding |
| `FeatureSearch` | tuist | Search, people and places | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-U10` | People index and cluster screens | Search and the clustered map landed; map granularity is still fixed |
| `FeatureTransfer` | tuist | The transfer centre, custody receipts, quota, storage and quarantine | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-U12` | The documented ladders and triage detail | Every screen landed and is routed; not all of the documented behaviour is built |
| `FeatureAuth` | tuist | Welcome, discovery, the device chooser, passphrase, enrolment ceremony and the device ledger | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-P2`, `S-P3`, `S-U13`, `S-U23` | A real auth service over Keychain (`S-P2`) | Built over `Preview*` doubles; both onboarding steps are scaffolds |
| `FeatureSharing` | tuist | Share links, drop inbox, peering, federation and moderation | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-U14`, `S-U22` | Inbound link redemption (`S-U22`) | Share detail is outstanding; the `https` deep-link parser lands `/s/` and `/u/` on a scaffold |
| `FeatureSettings` | tuist | The settings tree | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | — | Federation, advanced and about sections | Fifteen of eighteen sections landed; the Advanced mock-scenario switcher does not exist |
| `FeatureImport` | tuist | The picker, scan, plan, execution and history surfaces | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-P4`, `S-U11` | The import→seal→upload bridge (`S-P4`) | Per-run detail is outstanding, and `S-P4` waits on `S-P2`/`S-P3` |
| `FeatureCollections` | tuist | The sidebar collections — hidden, places and recently deleted | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-U20`, `S-U21` | Memories and duplicate review | `HiddenView` sits behind the SR1 local-auth gate, whose seam is a port |
| `Capsule` | tuist | The composition root — the thin app target shared by macOS, iOS and iPadOS | stabilizing | `mise run check-swift` | [capsule-swift/README](capsule-swift/README.md) | `S-U19` | Swap the mock adapter for the SDK one (`S-U19`) | `AppEnvironment.swift` is the single file where lane P and lane U meet |
| `capsule-docs` | bun | The Starlight documentation site, the design docs it publishes, and the `docs-truth` checks | stabilizing | `mise run check-docs` | [Developer Docs](capsule-docs/src/content/docs/design/developer-docs.md) | `S-Z8`, `S-Z9`, `S-Z10` | Generate the reference section (#415) | `mise run check-docs-truth` is deliberately outside both `check-docs` and `check-rust`; it needs no toolchain |
| `capsule-web` | bun | The browser client — guest drop, share viewer and the read-only gateway | stabilizing | `mise run check-web` | [Web Upload](capsule-docs/src/content/docs/design/web-upload.md) | `S-Q5` | Live-browser smokes (`S-Q5`, #409) | Every screen renders its empty state; there is no server to reach until #401 |
| `locales` | catalog | The canonical ICU MessageFormat catalogs — thirteen locales, the config and the schema | stabilizing | `mise run i18n-check` | [i18n](capsule-docs/src/content/docs/design/i18n.md) | — | Human review of the seeded entries | Roughly 350 machine-seeded entries across twelve locales are flagged in the `context` field and await review |
| `capsule-vision` | python | The vision and ML research notebooks behind the on-device tagging work | excluded | `mise run check-vision` | [AI](capsule-docs/src/content/docs/design/ai.md) | — | Post-v1 | Format and lint only — `check-vision` runs no notebook and no test, and nothing here ships in a client |
| `rawshift` | submodule | RAW decode, metadata extraction and derivative generation, in-house and out-of-tree | excluded | — | [Dependencies](capsule-docs/src/content/docs/design/dependencies.md) | — | A workspace dependency `capsule-core::media` can consume | A pinned submodule, not a workspace dependency; CI does not check it out and no gate here descends into it |
| `legacy-review/server-salvo` | review-bucket | The retired Salvo server, kept as the contract the Kynos rebuild must reproduce | review-only | — | [legacy-review/README](legacy-review/README.md) | — | Deleted once `capsule-server` reaches parity | Non-buildable reference material; nothing in the workspace links it |
| `legacy-review/sdk-progenitor` | review-bucket | The retired Progenitor SDK | review-only | — | [legacy-review/README](legacy-review/README.md) | — | Deleted once the spargen client covers it | Non-buildable reference material |
| `legacy-review/media-pipeline` | review-bucket | The retired `capsule_core::media` decode and derivative stack | review-only | — | [legacy-review/README](legacy-review/README.md) | — | Deleted once `capsule-core::media` lands on Rawshift (#410) | Non-buildable reference material; taking the decoder with it is why every still import is a `DeferredNoCodec` today |
| `legacy-review/core-import-media` | review-bucket | The quarantined twin of `capsule_core::exif` and the import executor's cancellation and progress halves | review-only | — | [legacy-review/README](legacy-review/README.md) | — | Deletion, which `S-C59` recorded and did not perform | All three modules are live, tested and newer in `capsule-core` than this snapshot, so the bucket is a stale twin rather than a quarantine |

## Deferred register

In scope, deliberately unscheduled. These are not packages, so they carry no row above; they
are listed here so `deferred` means something a reader can check.

| Item | Owner docs | State | Notes |
| --- | --- | --- | --- |
| Tethered camera import over PTP/IP | [Import — Pipeline](capsule-docs/src/content/docs/design/import/pipeline.md) | deferred | `S-B9`; the `ptpip-rs` crate does not exist yet |
| iCloud and Immich importers | [Import — Pipeline](capsule-docs/src/content/docs/design/import/pipeline.md) | deferred | `S-B7` and `S-B8`, both behind the Takeout adapter that landed |
| Passkey authentication | [Authentication](capsule-docs/src/content/docs/design/authentication.md) | deferred | Six Salvo operations that were in no document and always answered `CredentialNotFound`; dropped from v1 in `S-C56` |
| Live mDNS peering | [Peering](capsule-docs/src/content/docs/design/peering.md) | deferred | `S-E3` lands the in-process half; discovery on a real network is post-v1 |
| Native RTL layout | [i18n](capsule-docs/src/content/docs/design/i18n.md) | deferred | The catalogs and the twelve locales landed in `S-I2`; per-platform RTL layout did not |
| A browser MLS surface | [MLS Resilience](capsule-docs/src/content/docs/design/mls-resilience.md) | deferred | The `libcrux` provider has no `wasm32` target, so the `mls` feature is host-only |
