---
title: Self Hosting
description: Running capsule-server on PostgreSQL, Valkey and a filesystem blob store
status: draft
---

No release has been cut, so you build the server from source. The server stores only ciphertext and
never decodes media; clients do all processing and encryption.

## What you need

- **PostgreSQL 18**: the durable index (accounts, assets, quota).
- **Valkey**: upload sessions, auth sessions and counters. Required at every size; it is
  correctness state, not a cache. Run it with `--maxmemory-policy noeviction` (an evicting policy
  can make "revoke all sessions" revoke nothing) and persistence on.
- **A blob directory** (`BLOB_ROOT`) on **one filesystem**. Finalization is an atomic rename, so
  the tree cannot span volumes or Kubernetes PersistentVolumes.
- **A TLS-terminating reverse proxy** in front of the server.

`capsule-server/compose.yaml` runs Postgres and Valkey with the right settings for local use
(`mise run serve-deps`). Its credentials are for development only.

## Build

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

## Configure

Configuration is environment only; `capsule-server/.env.example` lists every variable. A durable
server refuses to start (exit 2), naming every missing variable at once, without:

| Variable | Purpose |
| --- | --- |
| `DATABASE_URL` | PostgreSQL connection |
| `VALKEY_URL` | Valkey connection |
| `BLOB_ROOT` | Blob directory |
| `JWT_ED25519_DER` | Base64 Ed25519 key that signs session tokens |
| `ATTESTATION_KEY_SEED` | Base64 seed that signs custody receipts; must be independent of the token key |

`SYNC_CURSOR_MAC_KEY` is derived from `JWT_ED25519_DER` when unset, so rotating the token key also
invalidates outstanding sync cursors unless you set it explicitly. Federation is off unless
`FEDERATION_ENABLED=true` and `FEDERATION_URL` are set; leave it off.

## Migrate, then start

`serve` never migrates and refuses a database schema older than the binary. Run migrations first, on every upgrade:

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` logs to stderr and prints `listening on <url>` to stdout. Do not use `--memory` in
production: it keeps everything except blobs in process memory.

## Maintenance

Schedule these one-shot commands (cron or a timer). They need `DATABASE_URL`, `VALKEY_URL` and
`--blob-root`, and default to a dry run:

- `capsule-server gc --apply`: reclaims unreferenced blobs after a grace window and sweeps
  upload-progress state. Run it regularly even when storage is not tight.
- `capsule-server purge --apply`: hard-purges soft-deleted assets whose retention has elapsed.
- `capsule-server scrub [--deep]`: read-only integrity check of the index against the blob store;
  **exits non-zero on any finding**, so alert on that. See
  [Maintenance](/design/filesystem/maintenance/#server-side-integrity-scrub).

## Backups

Durable state is PostgreSQL plus `BLOB_ROOT`; back up both, together. Valkey holds only sessions and
in-flight uploads, so losing it signs users out and restarts uploads but loses no library data.
Keep the key variables above in your secret store as well: they are not in the database, and
the server's signing and attestation identity depends on them.
