# Capsule SDK

The previous Progenitor client is quarantined under `legacy-review/sdk-progenitor/` and is not a
Cargo package. The replacement SDK is generated with Spargen from the canonical Kynos
REST/OpenAPI document, then wrapped by small Capsule-owned workflow modules.

## Development

- The typed REST client is generated from the server's OpenAPI **3.1** schema by
  [`spargen`](https://crates.io/crates/spargen), our in-house generator (slice `S-D8` in the
  repo-root `SLICES.md`). It has shipped and is published on crates.io; this repository pins `0.1.0` as a
  build-dependency (newer releases exist upstream).
  The previous progenitor pipeline required a lossy 3.1→3.0 schema down-conversion and is
  gone — we do not downgrade schemas.
- `AuthenticatedClient` is live: it wraps the generated `Client` and composes the token,
  protocol-version, and retry behaviour on top of it. Reach for the generated client directly
  only when you need a surface the wrapper does not cover.
- What the SDK is waiting on is the **Kynos OpenAPI contract**, not spargen. The generated
  surfaces track whichever OpenAPI document is committed here; they are regenerated once the
  replacement server publishes its schema.
