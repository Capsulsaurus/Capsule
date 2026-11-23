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

- Datetime: `jiff`, never `chrono`. chrono reaches the lock file only through `sea-orm-migration`'s defaults in `capsule-server-migration`, a server dev-dependency; no workspace crate names it (`cargo xtask architecture-check` gates the server).
- Errors: `thiserror` in libraries, `eyre`/`color-eyre` in binaries; no `anyhow`.
- Logging: `tracing`, never the `log` facade in new code.
- TLS: `rustls` only; never native-tls/openssl.
- Unit tests sit next to their code but in their own file: `foo.rs` ends with `#[cfg(test)] mod tests;` and the tests live in `foo/tests.rs` (`use super::*` keeps private access). No inline `mod tests { … }` blocks. `<crate>/tests/` holds public-API integration tests only. `mise run test-layout-check` enforces it.

## Docs

Follow:

- [Testing](../../capsule-docs/src/content/docs/development/testing.md) — test layers and layout.
- [Versioning — System Invariants](../../capsule-docs/src/content/docs/design/versioning.md#system-invariants).

Keep current:

- The crate-level `//!` docs — when a crate, an inter-crate edge or a server module is added, removed or renamed.
- The rationale comment beside each pin — see [dependencies.md](dependencies.md) for manifest changes.
- [Rust SDK reference](../../capsule-docs/src/content/docs/reference/sdk/rust.md) — when `capsule-sdk`'s module layout or public role changes; rustdoc itself publishes from master (`mise run doc-publish-rust`).
