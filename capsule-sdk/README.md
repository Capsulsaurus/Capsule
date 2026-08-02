# Capsule SDK

The previous Progenitor client is quarantined under `legacy-review/sdk-progenitor/` and is not a
Cargo package. The replacement SDK will be generated with Spargen from the canonical Kynos
REST/OpenAPI document, then wrapped by small Capsule-owned workflow modules.

## Development

- The typed REST client is generated from the server's OpenAPI **3.1** schema by `spargen`,
  our in-house generator (in development; slice `S-D8` in the repo-root `SLICES.md`).
  The previous progenitor pipeline required a lossy 3.1→3.0 schema down-conversion and is
  gone — we do not downgrade schemas. Its `AuthenticatedClient` and workflow ideas remain
  review-only until Spargen and the Kynos contract are ready.
