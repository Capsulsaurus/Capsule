# Capsule

## Code Style

- Self-validation: Most if not all code should be modular, reusable, and testable. The code that requires human review and manual testing should be minimal and focused on user facing features. All critical code must be primarily covered by complete and automated tests.
- Contract-driven development: Define the interfaces and data structures first, along with all test cases, before implementing the actual logic.
- Cohesion: All code should be split into cohesive modules that have a single responsibility and clear interfaces. Encapsulate unnecessary details.
- Minimalism: Choose to use a dependency if it reduces the scope of testing and quantity of code and as long as it does not compromise on performance and required capabilities.
- Traceability: all critical processes are verbosely logged so it is clear what happened after the fact and recovery can be feasible. Use INFO logs where necessary and DEBUG,TRACE aggressively for all critical processes. Logs should be structured and easily queryable. Instrument hot paths (e.g. major functions) for performance monitoring and debugging in production.
- Mocking: Use mocks for all external dependencies and critical internal processes. This allows us to have deterministic tests and easily simulate edge cases and failure scenarios that are hard to reproduce with real dependencies. Do not try to wire up two incomplete complex systems to mock each other.
- Linting and formatting is setup to be strict. Pre-commit and pre-push hooks are configured so you won't be able to push code that doesn't meet the standards.

## Dependencies

- Datetime: `jiff`, never `chrono` — chrono exists only as the sea-orm column type inside `capsule-cli/entity` (convert at the entity boundary).
- Errors: `thiserror` in libraries, `eyre`/`color-eyre` in binaries; no `anyhow`.
- Logging: `tracing`, never the `log` facade in new code.
- TLS: `rustls` only; never native-tls/openssl.
- Identifiers: UUIDv7 for new ids; UUIDv4 only where creation time must not leak.
- The canonical table (all platforms, exceptions, rationale) is the [Dependencies design doc](capsule-docs/src/content/docs/design/dependencies.md) — add a row there before introducing a dependency for a new domain.
- Licences: never add a `GPL-*`, `LGPL-*`, or `AGPL-*` dependency — `deny.toml` fails the build, and a copyleft crate in a statically-linked binary forecloses app-store distribution. Elect a permitted arm of a multi-licensed crate. Anything beyond attribution needs a `deny.toml` exception plus a root `NOTICE` entry, per the [Licensing design doc](capsule-docs/src/content/docs/design/licensing.md).

## Internationalization

- No hardcoded user-facing strings. Every translatable string is a key in the canonical catalogs under `locales/` (ICU MessageFormat). Add the key there, not inline in app code.
- After editing `locales/`, run `mise run i18n` to regenerate the per-platform files (Rust bundle, web JSON, Android `strings.xml`, iOS `.xcstrings`). Generated files are committed and carry a "do not edit by hand" banner; `mise run i18n-check` (part of `check-rust`) fails on drift.
- Keys use dotted namespaces (`area.subarea.name`). Server errors carry a stable `code` from the `error.*` namespace (referenced via `capsule_i18n::error_codes`); clients localize the code while the English detail message stays English.
- Language and region are separate settings in every app: language picks the catalog, region drives date, number and unit formatting. Both default from the OS.
- The CLI and TUI are English-only: their strings are inline English and the shared `error.*` codes render through the English catalog. The catalog rule above applies to the apps, the web client and the server.
- See the [i18n design doc](capsule-docs/src/content/docs/design/i18n.md) for the full contract and `locales/README.md` for the contributor workflow.

## Client & UI

- Platform tiers: **iOS and Android phone stabilize first**, then iPadOS and Android tablet, then macOS; Windows and Linux are deferred. This is an ordering, not a hierarchy — every native client is meant to end up fully featured and platform-integrated. The canonical table is [Clients — Platform Tiers](capsule-docs/src/content/docs/design/clients.md).
- What clients share: **iPadOS and macOS share views**; **iOS and Android share layouts and feature sets but not design language**; Apple logic is shared across iOS/iPadOS/macOS behind one route enum and one router, with shells chosen by size class rather than `#if os(...)`. A difference between two clients must be justified by a difference in the platform — "written first" is not a justification.
- Design language: shared skeleton, per-platform material. HIG and SF Symbols on Apple, Material on Android; neither emulates the other and neither invents a third house style. Bespoke components only for concepts the host platform has no analogue for.
- The **web client is permanently read-only** — authenticated viewing plus public share and guest-upload links — server-owned, and version-locked to its server. It is the one client allowed to assume its peer's exact version; native clients must stay lenient, which is why protocol evolution is additive.
- Catalog keys are namespaced by **surface, not by client**: a product concept present on more than one platform takes no platform prefix, because the generated Android and Apple catalogs are emitted from the same keys. Only genuinely platform-only chrome (a macOS menu bar) carries one. See [Internationalization](#internationalization).

## Rust Architecture Decisions

- The public server surface is Kynos REST with an **OpenAPI 3.2** contract, pinned with `openapi_as(SpecVersion::V3_2)`. Do not reintroduce Salvo, GraphQL or gRPC, and never emit or commit a 3.1 or 3.0 contract.
- Clients are generated with Spargen from the committed contract, never Progenitor. Everything that parses or serializes is generated; only orchestration over generated calls is hand-written, and the transfer protocol's resumable upload state machine is the whole of it.
- Media decoding, metadata extraction and derivatives come from Rawshift (`rawshift-image` from crates.io, never the submodule) through `capsule-core::media`. Codecs in `default` are pure Rust; native code enters only through the opt-in `media-linked-codecs` and `media-avif-decode` features. A format with no codec in the build is a typed `MediaError::UnsupportedFormat`, never a silent gap. HEIC decode is refused (libheif and libde265 are LGPL-3.0). Contract: [Thumbnails](capsule-docs/src/content/docs/design/thumbnails.md).
- LQIP is Chromahash 0.7.1, imported directly and implemented once in `capsule-core::lqip`. ThumbHash is retired; neither the crate nor the npm package may return.
- Blob storage and the [transfer protocol](capsule-docs/src/content/docs/design/transfer/index.md) are Capsule-owned behind narrow ports. Do not add `object_store` or generic CAS or transfer crates.
- Authentication state and upload-session state are separate ports whose adapters are `redis-rs` (Valkey) and in-memory, never PostgreSQL. PostgreSQL backs the durable ports (asset index, accounts, cohorts, quota). Do not introduce a generic TTL or CAS abstraction.
- Every change must preserve the [system invariants](capsule-docs/src/content/docs/design/versioning.md#system-invariants): a version mismatch may refuse a write, never lose or corrupt data.
- `legacy-review/` is non-buildable reference material. Restore code only after defining its contract and automated tests against the decisions above.

## Workflow

- Work is tracked in GitHub issues; [ROADMAP.md](ROADMAP.md) is the ordered path to the dogfood release. Issues labelled `track:deferred` are not drained automatically.
- Design docs state the contract: invariants, state machines and trust boundaries. Implementation detail belongs in code and rustdoc; history belongs in [`adr/`](adr/README.md). Do not add slice ids, issue archaeology or dated status notes to the docs.
- The CLI/TUI is the reference client: a feature lands there first, every user flow is reachable as a `--json` subcommand, and it is tested per the [testing plan](capsule-docs/src/content/docs/development/testing.md).
