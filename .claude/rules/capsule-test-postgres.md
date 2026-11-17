---
paths:
  - "capsule-test-postgres/**"
---

# capsule-test-postgres

## Rules

- Test support only: `capsule-server` and `capsule-e2e` take it as a dev-dependency, and no served binary may link it.
- It drives the container CLI and adds no container-API crate. testcontainers left over its unmaintained `astral-tokio-tar` (RUSTSEC-2026-0440), and the tiers need only one image, one published port and `exec`.

## Docs

Follow:

- [Testing](../../capsule-docs/src/content/docs/development/testing.md) — the container tier this harness serves.

Keep current:

- The `test-rust-containers` task comment in `mise.toml` and the `rust-containers` job in `.github/workflows/ci.yml` — when the CLI, image or the environment variables it reads change.
