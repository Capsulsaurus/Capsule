---
paths:
  - "capsule-server/**"
---

# capsule-server

## Rules

- The public server surface is Kynos REST with an **OpenAPI 3.2** contract, pinned with `openapi_as(SpecVersion::V3_2)`. Do not reintroduce Salvo, GraphQL or gRPC, and never emit or commit a 3.1 or 3.0 contract.
- Blob storage and the [transfer protocol](../../capsule-docs/src/content/docs/design/transfer/index.md) are Capsule-owned behind narrow ports. Do not add `object_store` or generic CAS or transfer crates.
- Authentication state and upload-session state are separate ports whose adapters are `redis-rs` (Valkey) and in-memory, never PostgreSQL. PostgreSQL backs the durable ports (asset index, accounts, cohorts, quota). Do not introduce a generic TTL or CAS abstraction.
- Errors carry a stable `code` from `capsule_i18n::error_codes`; the error-code contract is in [i18n.md](i18n.md).
- `capsule-server/openapi.json` is generated: regenerate with `mise run openapi-kynos`, never edit by hand (`openapi-check-kynos` fails on drift). The federation pull client in `build.rs` follows the Spargen rule in [capsule-sdk.md](capsule-sdk.md).

## Docs

Follow, everywhere in the crate:

- [API practices](../../capsule-docs/src/content/docs/development/api-practices.md) — layering and store conventions.
- [Networking](../../capsule-docs/src/content/docs/design/networking.md) — problem details and retry classes (`problem*`, `openapi/`).
- [Threat model — Validation](../../capsule-docs/src/content/docs/design/threat-model/validation.md) — server refuse-by-default checklist (`negotiation.rs`, `upload/`, `membership/`).

Follow, by module (`capsule-server/src/…`, with its `routes/` handlers):

| Module | Doc |
| --- | --- |
| `upload/`, `limits` | [Transfer](../../capsule-docs/src/content/docs/design/transfer/index.md), [Upload](../../capsule-docs/src/content/docs/design/transfer/upload.md) |
| `serve/`, `membership/` | [Download](../../capsule-docs/src/content/docs/design/transfer/download.md) |
| `verify/`, `attestation/` | [Durability](../../capsule-docs/src/content/docs/design/transfer/durability.md) |
| `blob/`, `store/`, `ledger/`, `index/`, `postgres/`, `gc/`, and the `capsule-server/migration/` crate | [Filesystem — Server](../../capsule-docs/src/content/docs/design/filesystem/server.md) |
| `scrub/`, `cli.rs` | [Filesystem — Maintenance](../../capsule-docs/src/content/docs/design/filesystem/maintenance.md), [Failure modes](../../capsule-docs/src/content/docs/design/cryptography/failure-modes.md) |
| `auth/`, `discovery/`, `config`, routes `auth`, `sessions`, `totp`, `oidc`, `profile`, `well_known` | [Authentication](../../capsule-docs/src/content/docs/design/authentication.md); `discovery/` also [Schema rules](../../capsule-docs/src/content/docs/design/threat-model/schema-rules.md) |
| `routes/ops.rs` | [Authorization](../../capsule-docs/src/content/docs/design/authorization.md) |
| `enrollment/` | [Device enrollment](../../capsule-docs/src/content/docs/design/device-enrollment.md) |
| `counter/` (attempt and rate budgets for auth, share, drop, enrollment and federation pulls) | [Device enrollment](../../capsule-docs/src/content/docs/design/device-enrollment.md), [Federation](../../capsule-docs/src/content/docs/design/federation.md) |
| `sync/` | [Sync](../../capsule-docs/src/content/docs/design/sync.md) |
| `federation/`, routes `federation`, `hints`, `subscriptions` | [Federation](../../capsule-docs/src/content/docs/design/federation.md) |
| `moderation/` | [Moderation](../../capsule-docs/src/content/docs/design/moderation.md) |
| `activity/` | [Activity log](../../capsule-docs/src/content/docs/design/activity-log.md) |
| `quota/` | [Quota](../../capsule-docs/src/content/docs/design/quota.md) |
| `share/` | [Share links](../../capsule-docs/src/content/docs/design/share-links.md) |
| `drop/` | [Web upload](../../capsule-docs/src/content/docs/design/web-upload.md) |
| `escrow/` | [Backup and recovery](../../capsule-docs/src/content/docs/design/backup-recovery.md) |
| `album/` | [Organization](../../capsule-docs/src/content/docs/design/organization.md) |
| `routes/roster.rs` | [MLS](../../capsule-docs/src/content/docs/design/cryptography/mls.md) |
| `routes/upgrade.rs` | [Versioning](../../capsule-docs/src/content/docs/design/versioning.md) |

Keep current:

- [API surfaces](../../capsule-docs/src/content/docs/design/api-surfaces.md) — its endpoint ↔ module ↔ owning-doc table, whenever a route is added, removed or moved (`check-docs-truth` runs the endpoint census against `openapi.json`; exemptions live in `capsule-docs/endpoint-census-allowlist.txt`).
- `capsule-server/openapi.json` (`mise run openapi-kynos`) and the hand-written [API reference overview](../../capsule-docs/src/content/docs/reference/api.md).
- [Module Map — Server Modules](../../capsule-docs/src/content/docs/design/module-map.md#server-modules) — a new or renamed top-level module.
- [Self-hosting guide](../../capsule-docs/src/content/docs/guides/self-hosting.md) — any change to `config`, `cli.rs`, `client_addr`, `compose.yaml`, `.env.example` or the migration procedure.
