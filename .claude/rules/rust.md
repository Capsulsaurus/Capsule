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

- Datetime: `jiff`, never `chrono`, which is out of the lock file. RUSTSEC-2026-0543 (unmaintained, every version) makes `mise run deny-check` refuse it if anything brings it back; the usual way in is a crate's default datetime feature, which is why the workspace `sea-orm` pin has its defaults off and the server's migrator is its own rather than `sea-orm-migration`.
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
