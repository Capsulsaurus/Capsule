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
  the tree cannot span volumes or Kubernetes PersistentVolumes. It holds the blobs and, under
  `ledger/`, each owner's signed ledger, which is what lets the index be rebuilt from it.
- **A TLS-terminating reverse proxy** in front of the server.

`capsule-server/compose.yaml` runs Postgres and Valkey with the right settings for local use
(`mise run serve-deps`). Its credentials are for development only.

## Build

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

## Configure

Configuration is environment only; `capsule-server/.env.example` is a starting template, and this
page names the variables an operator must decide. A durable
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

Name your reverse proxy so the per-source rate limits see the client rather than the proxy:
`TRUSTED_PROXIES` takes a comma-separated list of addresses and CIDR networks (`10.0.0.0/8`), and
`TRUSTED_PROXY_HOPS` trusts that many forwarding hops nearest the server, whatever their address.
Unset, no `Forwarded` or `X-Forwarded-For` header is believed. That is safe, but every client
behind the proxy then shares the proxy's budget.

Once you name a proxy, it must strip or overwrite any `Forwarded` header the client sent. The
server reads `Forwarded` in preference to `X-Forwarded-For`, and it believes an element that a
named proxy passed on. A client-sent `Forwarded` that reaches the server unchanged therefore lets
the client choose its own rate-limit key, and fill the per-source table with keys it invents. nginx
passes the header through by default. Drop it there, and send the address the proxy saw in
`X-Forwarded-For`:

```nginx
proxy_set_header Forwarded "";
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

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

After restoring a backup, or losing the database, run `capsule-server rebuild` to compare the
index with the owner ledgers under `BLOB_ROOT`, then, with `serve` stopped,
`capsule-server rebuild --apply` to restore the rows the index lacks. It never overwrites a row
the index holds. Set `ATTESTATION_KEY_SEED`
on the host so it verifies the ledgers' signatures. See
[Rebuild](/design/filesystem/server/#rebuild).

## Backups

Durable state is PostgreSQL plus `BLOB_ROOT` (blobs and `ledger/`); back up both, together. Valkey holds only sessions and
in-flight uploads, so losing it signs users out and restarts uploads but loses no library data.
Keep the key variables above in your secret store as well: they are not in the database, and
the server's signing and attestation identity depends on them.

Take the two in this order, the database first:

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` may keep running. A blob is on disk, and its ledger entry appended, before the index row
that names it commits, so a blob copy taken after the dump holds every blob the dump references.
It can also hold writes the dump missed; the rebuild below restores those rows. Do not run
`gc --apply` or `purge --apply` while the backup runs: either can remove a blob the dump still
references.

Valkey needs no backup. If you keep one anyway (its `appendonly` files under `/data`), restore it
only beside the database dump it was taken with.

### Restoring onto a new host

1. Start an empty PostgreSQL and an empty Valkey, and set the same `JWT_ED25519_DER` and
   `ATTESTATION_KEY_SEED` as the old host.
2. Restore the database. The dump carries the schema and its migration record, so do not run
   `capsule-server-migration up` first:
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Copy the blob directory back to `BLOB_ROOT`, on one filesystem.
4. With `serve` stopped, run `capsule-server rebuild` and read its report. It lists the rows
   written after the dump; `capsule-server rebuild --apply` restores them with the sequence
   numbers they had, so clients' sync cursors stay valid.
5. Run `capsule-server scrub --deep`. It should exit zero.
6. Start `serve`. Everyone signs in again, because their sessions were in Valkey. Clients resume
   syncing where they stopped.

If the database is lost and there is no dump, migrate an empty database and run
`capsule-server rebuild --apply` over the blob directory. It restores every asset row from the
owner ledgers. Accounts, published device directories, album membership and share links are in
the database only, and a rebuild does not restore them.

Rehearse this before you trust real data to it. One way is to check with `capsule restore` that
every original comes back byte for byte. `capsule-e2e/tests/backup_restore_drill.rs` runs the
same steps against containers (`mise run test-rust-containers`).
