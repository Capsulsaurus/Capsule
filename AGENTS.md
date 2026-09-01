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

- Datetime: `jiff`, never `chrono` — chrono exists only as the sea-orm column type inside `capsule-cli/entity` (convert at the entity boundary). The server's copy went to `legacy-review/` with the Salvo tree.
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
- See the [i18n design doc](capsule-docs/src/content/docs/design/i18n.md) for the full contract and `locales/README.md` for the contributor workflow.

## Rust Architecture Decisions

- The public server surface is Kynos REST/OpenAPI only. Do not reintroduce Salvo, GraphQL, or gRPC. The served document is **OpenAPI 3.2**: enabling Kynos's `openapi32` feature does not by itself produce one — `capsule-server` pins it with `openapi_as(SpecVersion::V3_2)`. Never emit or commit a 3.1 or 3.0 contract.
- Generate clients with Spargen from the checked-in Kynos OpenAPI contract. Do not use Progenitor. Everything that parses or serializes is generated — every body, every typed parameter, and the byte-serving endpoints. Only *orchestration over* generated calls is hand-written, and the resumable upload state machine (`S-D1`) is the whole of it; do not hand-write a second parser.
- Rawshift owns media decoding, metadata extraction, and derivative generation. Capsule imports **Chromahash 0.7.1** directly — the earlier "after its v1 release" gate is amended to that release — and Rawshift must not wrap it. LQIP encode/decode lives in its own `capsule-core::lqip` module (slice `S-B14`), outside the retiring `capsule-core::media` stack, so one implementation serves the import pipeline, the FFI, and `capsule-wasm`. **ThumbHash is retired**: neither the `thumbhash` crate nor the npm `thumbhash` package may be reintroduced. Contract: [Thumbnails — LQIP](capsule-docs/src/content/docs/design/thumbnails.md#lqip).
- Blob storage and resumable encrypted upload remain Capsule-owned behind narrow, arbitrary-backend ports. Do not add `object_store` or generic CAS/transfer crates without revisiting the security contract.
- Keep authentication state and upload-session state as separate Capsule ports with PostgreSQL, `redis-rs`, and in-memory adapters. Do not introduce a generic TTL/CAS abstraction.
- `legacy-review/` is non-buildable reference material. Restore code only after defining its contract and automated tests against the decisions above.
