---
paths:
  - "capsule-sdk/**"
---

# capsule-sdk

## Rules

- Clients are generated with Spargen from the committed contract, never Progenitor. Everything that parses or serializes is generated; only orchestration over generated calls is hand-written, and the transfer protocol's resumable upload state machine is the whole of it.
- The SDK owns network state; local state belongs to `capsule-library` and pure logic to `capsule-core` ([Module Map — Stateless Core](../../capsule-docs/src/content/docs/design/module-map.md#stateless-core)).
- Clients switch on the `capsule_i18n::error_codes` constants, never bare HTTP statuses; the error-code contract is in [i18n.md](i18n.md).
- `capsule-sdk/bindings-surface.json` is the committed UniFFI surface: regenerate with `mise run gen-bindings`, checked by `gen-bindings --check`.

## Docs

Follow, by module (`capsule-sdk/src/…`):

| Module | Doc |
| --- | --- |
| `upload`, `staged` | [Transfer](../../capsule-docs/src/content/docs/design/transfer/index.md), [Upload](../../capsule-docs/src/content/docs/design/transfer/upload.md) |
| `fetch` | [Download](../../capsule-docs/src/content/docs/design/transfer/download.md), [Encryption](../../capsule-docs/src/content/docs/design/cryptography/encryption.md) |
| `verify` | [Durability](../../capsule-docs/src/content/docs/design/transfer/durability.md) |
| `net/` | [Networking](../../capsule-docs/src/content/docs/design/networking.md); `net/negotiation.rs` also [Versioning](../../capsule-docs/src/content/docs/design/versioning.md) |
| `upgrade` | [Versioning](../../capsule-docs/src/content/docs/design/versioning.md) |
| `sync` | [Sync](../../capsule-docs/src/content/docs/design/sync.md) |
| `auth`, `cohort/` | [Authentication](../../capsule-docs/src/content/docs/design/authentication.md) |
| `peering` | [Peering](../../capsule-docs/src/content/docs/design/peering.md), [Keys](../../capsule-docs/src/content/docs/design/cryptography/keys.md) |
| `federation` | [Federation](../../capsule-docs/src/content/docs/design/federation.md) |
| `share/` | [Share links](../../capsule-docs/src/content/docs/design/share-links.md) |
| `recovery/` | [Backup and recovery](../../capsule-docs/src/content/docs/design/backup-recovery.md) |
| `albums` | [Organization](../../capsule-docs/src/content/docs/design/organization.md), [Threat model — Validation](../../capsule-docs/src/content/docs/design/threat-model/validation.md) |
| `push` (pushes a locally imported asset to the server) | [Upload](../../capsule-docs/src/content/docs/design/transfer/upload.md), [Organization](../../capsule-docs/src/content/docs/design/organization.md), [Threat model — Validation](../../capsule-docs/src/content/docs/design/threat-model/validation.md) |
| `cache_budget` | [Filesystem — Client](../../capsule-docs/src/content/docs/design/filesystem/client.md) |

Keep current:

- [SDK reference](../../capsule-docs/src/content/docs/reference/sdk.md) and [Rust SDK reference](../../capsule-docs/src/content/docs/reference/sdk/rust.md) — client, `ffi`, sync or upload surface changes.
- `capsule-sdk/bindings-surface.json` (`mise run gen-bindings`).
