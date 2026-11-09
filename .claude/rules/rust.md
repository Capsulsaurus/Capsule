---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - "Cargo.lock"
  - "clippy.toml"
  - "rustfmt.toml"
  - "rust-toolchain.toml"
---

# Rust

## Rules

- Datetime: `jiff`, never `chrono` — chrono exists only as the sea-orm column type inside `capsule-cli/entity` (convert at the entity boundary).
- Errors: `thiserror` in libraries, `eyre`/`color-eyre` in binaries; no `anyhow`.
- Logging: `tracing`, never the `log` facade in new code.
- TLS: `rustls` only; never native-tls/openssl.
- Unit tests sit next to their code but in their own file: `foo.rs` ends with `#[cfg(test)] mod tests;` and the tests live in `foo/tests.rs` (`use super::*` keeps private access). No inline `mod tests { … }` blocks. `<crate>/tests/` holds public-API integration tests only. `mise run test-layout-check` enforces it.

## Docs

Follow:

- [Testing](../../capsule-docs/src/content/docs/development/testing.md) — test layers and layout.
- [Versioning — System Invariants](../../capsule-docs/src/content/docs/design/versioning.md#system-invariants).

Keep current:

- [Module Map](../../capsule-docs/src/content/docs/design/module-map.md) — when a crate, an inter-crate edge or a server module is added, removed or renamed.
- [Dependencies](../../capsule-docs/src/content/docs/design/dependencies.md) — see [dependencies.md](dependencies.md) for manifest changes.
- [Crates reference](../../capsule-docs/src/content/docs/reference/crates.md) — when a crate's public role changes.
