---
paths:
  - "xtask/**"
  - "mise.toml"
  - "mise-tasks/**"
  - "hk.pkl"
  - ".github/workflows/**"
  - ".config/**"
---

# Build, CI and repository tooling

## Docs

Keep current:

- [Local development](../../capsule-docs/src/content/docs/development/local-development.md) — the task graph and gates, for any task added, renamed or removed.
- [`CONTRIBUTING.md`](../../CONTRIBUTING.md) — workflow, commit rules and the release process (`.github/workflows/prepare-release.yml`, `release.yml`, `xtask` set-version).
- [Developer docs](../../capsule-docs/src/content/docs/design/developer-docs.md) — how reference pages and surface files are produced (`xtask/src/bindings_surface.rs`, `xtask/src/wasm_surface.rs`, `mise-tasks/gen-bindings`, `.github/workflows/ci.yml`).
- [Dependencies](../../capsule-docs/src/content/docs/design/dependencies.md) and [Module Map](../../capsule-docs/src/content/docs/design/module-map.md) — what `xtask/src/architecture.rs` enforces.
- [Testing](../../capsule-docs/src/content/docs/development/testing.md) — what `xtask/src/test_layout.rs` and `.config/nextest.toml` enforce.
- [Clients — Test and Performance Tooling](../../capsule-docs/src/content/docs/design/clients.md#test-and-performance-tooling) — `xtask/src/bench_apps.rs`, `mise-tasks/bench-apps`, `.github/workflows/bench-apps.yml`.
- [i18n](../../capsule-docs/src/content/docs/design/i18n.md) — `xtask/src/i18n.rs`, `xtask/src/i18n_guard.rs`, `xtask/src/translate_readme.rs` (see [i18n.md](i18n.md)).
