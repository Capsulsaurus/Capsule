---
paths:
  - "capsule-cli/**"
---

# capsule-cli

## Rules

- This is the reference client: every user flow is reachable as a `--json` subcommand (see the root `AGENTS.md` Workflow section). Strings are inline English; see [i18n.md](i18n.md) for the `error.*` codes.
- `capsule-cli/cli-surface.json` is generated from the clap definitions: regenerate with `mise run cli-surface`, checked by `cli-surface-check`.

## Docs

Follow:

- [Testing](../../capsule-docs/src/content/docs/development/testing.md) — the CLI/TUI test plan, including `tests/json_contract.rs` and `src/tui/`.
- By module (`capsule-cli/src/…` unless rooted): `peer`, `peering_state` → [Peering](../../capsule-docs/src/content/docs/design/peering.md); `db` (`capsule.sqlite`: the CLI's SQLite tables and their `user_version` migrator) → [Peering](../../capsule-docs/src/content/docs/design/peering.md) for the peering tables, [Sync](../../capsule-docs/src/content/docs/design/sync.md) for `sync_cursor` and `synced_assets`; `verify` → [Durability](../../capsule-docs/src/content/docs/design/transfer/durability.md); `camera_discovery`, `camera_pairing`, `camera_import` → [Camera import](../../capsule-docs/src/content/docs/design/import/camera-import.md); `federation` → [Federation](../../capsule-docs/src/content/docs/design/federation.md); `syncstore` → [Sync](../../capsule-docs/src/content/docs/design/sync.md); `cache` → [Filesystem — Client](../../capsule-docs/src/content/docs/design/filesystem/client.md); `repair` → [Filesystem — Maintenance](../../capsule-docs/src/content/docs/design/filesystem/maintenance.md); `restore` → [Backup and recovery](../../capsule-docs/src/content/docs/design/backup-recovery.md); `cull.rs` → [Organization](../../capsule-docs/src/content/docs/design/organization.md).

Keep current:

- `capsule-cli/cli-surface.json` (`mise run cli-surface`) and the hand-written [CLI reference overview](../../capsule-docs/src/content/docs/reference/cli.md).
- [Getting started](../../capsule-docs/src/content/docs/guides/getting-started.md) and the [User guide](../../capsule-docs/src/content/docs/guides/user-guide.md) — any command or output a user follows.
- [Google Photos migration guide](../../capsule-docs/src/content/docs/guides/google-photos-migration.md) — the takeout import command (`tests/takeout_import.rs`).
