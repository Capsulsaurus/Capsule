---
paths:
  - "**/Cargo.toml"
  - "Cargo.lock"
  - "deny.toml"
  - "**/package.json"
  - "gradle/libs.versions.toml"
  - "**/build.gradle.kts"
  - "capsule-swift/{Project,Tuist}.swift"
  - "capsule-core-swift/Package.swift"
  - "NOTICE"
---

# Dependencies

## Rules

- The canonical table (all platforms, exceptions, rationale) is the [Dependencies design doc](../../capsule-docs/src/content/docs/design/dependencies.md) — add a row there before introducing a dependency for a new domain.
- Licences: never add a `GPL-*`, `LGPL-*`, or `AGPL-*` dependency — `deny.toml` fails the build, and a copyleft crate in a statically-linked binary forecloses app-store distribution. Elect a permitted arm of a multi-licensed crate. Anything beyond attribution needs a `deny.toml` exception plus a root `NOTICE` entry, per the [Licensing design doc](../../capsule-docs/src/content/docs/design/licensing.md).
- Refused by a scoped rule elsewhere — check the owning rule before adding: `chrono` outside `capsule-cli/entity`, `anyhow`, `log`, native-tls/openssl ([rust.md](rust.md)); Salvo, GraphQL, gRPC, `object_store`, generic CAS/TTL/transfer crates ([capsule-server.md](capsule-server.md)); Progenitor ([capsule-sdk.md](capsule-sdk.md)); ThumbHash (crate or npm), the `rawshift` submodule as a path dependency, libheif/libde265 ([capsule-core.md](capsule-core.md)).

## Docs

Keep current:

- [Dependencies](../../capsule-docs/src/content/docs/design/dependencies.md) — version pins and the per-platform table; `mise run architecture-check` enforces part of it.
- [Licensing](../../capsule-docs/src/content/docs/design/licensing.md) and the root `NOTICE` — for any licence beyond plain attribution; `mise run license-check` runs `cargo deny`.
