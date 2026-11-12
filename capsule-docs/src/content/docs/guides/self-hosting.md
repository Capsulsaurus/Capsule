---
title: Self Hosting
description: Running capsule-server on PostgreSQL and a filesystem blob store
status: draft
---

No release has been cut, so you build the server from source. The server stores only ciphertext and
never decodes media; clients do all processing and encryption.

## What you need

- **PostgreSQL 18**: the durable index (accounts, assets, quota) and the server's expiring state
  (sign-in sessions, upload sessions, rate counters). It is the only database the server uses.
- **A blob directory** (`BLOB_ROOT`) on **one filesystem**. Finalization is an atomic rename, so
  the tree cannot span volumes or Kubernetes PersistentVolumes. It holds the blobs and, under
  `ledger/`, each owner's signed ledger, from which the index can be rebuilt. A network mount such
  as NFS works if it supports file locks. `serve` and `purge --apply` lock each owner's
  `ledger/<owner>/.lock` before appending, so a mount with locking disabled (`nolock`) lets them
  interleave unless both run on the same host.
- **A TLS-terminating reverse proxy** in front of the server, configured to:
  - accept request bodies of at least 16 MiB, the largest upload chunk;
  - stream request bodies to the server rather than buffering them;
  - disable TLS early data (0-RTT), or forward it with the `Early-Data: 1` header so the server can
    refuse a write;
  - apply no total-duration timeout to a request body, and an idle timeout longer than the
    server's stall bound, so a slow but steady chunk is never cut.

  HTTP/2 and HTTP/3 on the client side of the proxy are optional; clients fall back to HTTP/1.1.

## Hardware

These figures are for one node running the server and PostgreSQL together. They are sizing
guidance derived from what the server does, not benchmarks.

The server's work is light, so most of the hardware budget should go to **storage**. It hashes
each chunk with SHA-256 as it arrives and re-hashes at finalize and during `scrub --deep`. It
verifies the client's hybrid signature (Ed25519 and ML-DSA-65) at finalize, signs receipts the same
way, and runs Argon2id once per password login (about 19 MiB each). It never decodes, resizes or
transcodes media, never encrypts or decrypts, and leaves TLS to the reverse proxy. An upload's body
is never buffered beyond one chunk, so memory does not grow with file size.

| | Minimum | Recommended |
| --- | --- | --- |
| **CPU** | 2 cores, x86_64 or arm64 | 4 cores. SHA-2 instructions (SHA-NI on x86_64, the Armv8 crypto extensions) speed up hashing |
| **RAM** | 2 GB | 4 to 8 GB. More helps PostgreSQL's cache on a large library, but storage pays back first |
| **System storage** | SSD, 10 GB free | SSD or NVMe for PostgreSQL |
| **Blob storage** | Your library's size, plus room for uploads in flight | Redundant disks (mirror, RAID or ZFS) sized for several years of growth. Spinning disks are fine |
| **Network** | 10 Mbit/s upstream | Gigabit on the LAN, and 50 Mbit/s or more upstream if devices sync from outside it |
| **Power** | — | A UPS, so a power cut does not interrupt PostgreSQL mid-write |

- **`BLOB_ROOT` stays on one filesystem.** The whole tree, `incoming/` included, must be one
  filesystem, so it cannot be split across an SSD and a disk array or across two ZFS datasets. Put
  it all on the array.
- **Give PostgreSQL the memory the server does not use.** Every authenticated request reads its
  session from PostgreSQL and every upload chunk records its range there, so its cache serves the
  request path as well as the library index. Sessions, upload sessions and counters are small, and
  the server deletes them once expired.
- **Upstream bandwidth is the limit for remote devices.** A phone outside your network uploads at
  your connection's *download* speed, but every fetch of a preview or original leaves at your
  *upstream* speed. A chunk is cut only when it stops arriving, never for taking long, and a client
  sizes its chunks to finish in a short target time at the speed it measures, so a slow link uploads
  at its own rate. Keep the server's stall bound below your proxy's idle timeout, so the server
  answers a stalled chunk before the proxy cuts it.
- **Size the disks for originals, not derivatives.** Derivatives are a small fraction of a
  library of RAW files and video. The server keeps each blob once, as the client encrypted it.
- **Users add bandwidth, not compute.** Search, thumbnails and organization run on the clients.
  More people browsing means more bytes served, but little more CPU or RAM.

`capsule-server/compose.yaml` runs Postgres with the right settings for local use
(`mise run serve-deps`). Its credentials are for development only.

## Build

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

## Configure

Configuration is environment only. `capsule-server/.env.example` is a starting template, and this
page names the variables an operator must decide. A durable server refuses to start (exit 2),
naming every missing variable at once, without:

| Variable | Purpose |
| --- | --- |
| `DATABASE_URL` | PostgreSQL connection |
| `BLOB_ROOT` | Blob directory |
| `JWT_ED25519_DER` | Base64 Ed25519 key that signs session tokens |
| `ATTESTATION_KEY_SEED` | Base64 seed that signs custody receipts; must be independent of the token key |

`SYNC_CURSOR_MAC_KEY` is derived from `JWT_ED25519_DER` when unset, so rotating the token key also
invalidates outstanding sync cursors unless you set it explicitly. Federation is off unless
`FEDERATION_ENABLED=true` and `FEDERATION_URL` are set; leave it off. `VALKEY_URL` is no longer
read: a value left over from an older setup is ignored with a warning.

Name your reverse proxy so the per-source rate limits see the client rather than the proxy.

| Variable | Meaning |
| --- | --- |
| `TRUSTED_PROXIES` | Comma-separated addresses and CIDR networks (`10.0.0.0/8`). A network of `/0` is refused, because it would believe every sender. |
| `TRUSTED_PROXY_HOPS` | Trusts that many forwarding hops nearest the server, whatever their address. |
| `TRUSTED_PROXY_HEADER` | The one forwarding field the server reads: `x-forwarded-for` (the default) or `forwarded` (RFC 7239). Any other value refuses to start. |

Unset, no forwarding header is believed. That is safe, but every client behind the proxy then
shares the proxy's budget. The field `TRUSTED_PROXY_HEADER` does not name is ignored whoever sends
it, so a client cannot choose its own rate-limit key by sending one that your proxy passes on. Name
the field your proxy appends to. With the default, have your proxy append the address it saw to
`X-Forwarded-For`, as nginx does with:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Set `TRUSTED_PROXY_HEADER=forwarded` only if your proxy appends its own element to `Forwarded`
and does not write `X-Forwarded-For`.

A session lasts `SESSION_TTL_DAYS` (default 180) without an access-token issuance, and never
longer than `SESSION_HARD_CAP_DAYS` (default 365) from sign-in. The server refuses to start if
either is zero or negative, or if the cap is shorter than the window. See
[session expiry](/design/authentication/#session-expiry-and-revocation). To make your identity
provider the only way to sign in, set `PASSWORD_AUTH_DISABLED=true`; `serve` then refuses to start
without a configured provider (`OIDC_ISSUER` and `OIDC_CLIENT_ID`, as in `.env.example`).

## Migrate, then start

`serve` never migrates and refuses a database schema older than the binary. Run migrations first,
on every upgrade:

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` logs to stderr and prints `listening on <url>` to stdout. Do not use `--memory` in
production: it keeps everything except blobs in process memory.

## Maintenance

Schedule these one-shot commands (cron or a timer). They need `DATABASE_URL` and `--blob-root`, and
default to a dry run:

- `capsule-server gc --apply`: reclaims unreferenced blobs after a grace window and deletes
  expired sessions, upload sessions and counters. Run it regularly even when storage is not tight.
- `capsule-server purge --apply`: hard-purges soft-deleted assets whose retention has elapsed. It
  also needs the server's `ATTESTATION_KEY_SEED`, because each purge is signed into the
  owner ledger ([](/design/filesystem/server/#the-owner-ledger)) first, and refuses without it. It
  exits non-zero when the ledger could not record a purge; that asset is kept for the next run.
- `capsule-server scrub [--deep]`: read-only integrity check of the index against the blob store.
  It **exits non-zero on any finding**, so alert on that
  ([](/design/filesystem/maintenance/#server-side-integrity-scrub)).

After restoring a backup or losing the database, rebuild the index from the owner ledgers under
`BLOB_ROOT` ([](/design/filesystem/server/#rebuild)). Set `ATTESTATION_KEY_SEED` on the host so it
verifies the ledgers' signatures.

1. `capsule-server rebuild` compares the index with the ledgers.
2. With `serve` stopped, `capsule-server rebuild --apply` restores the rows the index lacks. It
   never overwrites a row the index holds. It re-anchors a ledger that lost its tail, after which
   the server writes to that owner again.
3. If the report says a ledger was rewritten, the server refuses that owner's writes until you have
   investigated and run `capsule-server rebuild --apply --accept-rewrites`.

A server that applied lifecycle ops (delete, restore, metadata update, derivative, replace) before
it recorded their blobs' sizes holds those references at size 0. Run
`capsule-server repair-sizes` once to list them, then `capsule-server repair-sizes --apply` to fill
each size in from the blob store.
It is safe beside a running `serve`, and each repaired asset is sent to clients again. It exits
non-zero when a blob is missing or empty and leaves that reference as it was.

## Backups

Durable state is PostgreSQL plus `BLOB_ROOT` (blobs and `ledger/`); back up both, together. The
database also holds sign-in sessions and in-flight upload sessions, so a restore keeps users signed
in. Keep the key variables above in your secret store too: they are not in the
database, and the server's signing and attestation identity depends on them.

Take the database first, then the blobs:

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` may keep running. A blob is on disk and its ledger entry appended before the index row that
names it commits, so a blob copy taken after the dump holds every blob the dump references, and
possibly writes the dump missed. Do not run `gc --apply` or `purge --apply` while the backup runs:
either can remove a blob the dump still references.

The rebuild below restores the assets uploaded between the dump and the blob copy, because it adds
every row the index lacks. It never changes a row the dump already holds, so a delete, restore or
metadata edit made to an older asset in that window is not applied. A rebuild reports such a change
as a row that differs (and exits non-zero) only when it changed the asset's blobs; a delete or
restore goes unreported. When a backup must capture every change, stop `serve` while you take both
halves. If a restore reports rows that differ and the old host still runs, take the backup again
that way.

### Restoring onto a new host

1. Start an empty PostgreSQL, and set the same `JWT_ED25519_DER` and
   `ATTESTATION_KEY_SEED` as the old host.
2. Restore the database. The dump carries the schema and its migration record, so do not run
   `capsule-server-migration up` first:
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`.
3. Copy the blob directory back to `BLOB_ROOT`, on one filesystem.
4. With `serve` stopped, run `capsule-server rebuild` and read its report. It lists the assets
   uploaded after the dump; `capsule-server rebuild --apply` restores them with the sequence
   numbers they had, so clients' sync cursors stay valid.
5. Run `capsule-server scrub --deep`. It should report nothing; it exits non-zero on any finding.
6. Start `serve`. Sessions come back with the database, so a client signed in at the dump stays
   signed in unless its session has since expired. Before it listens, `serve` discards every
   unfinished upload session whose staged file under `BLOB_ROOT` was not restored, and deletes
   every staged file with no session; a client whose upload was discarded starts that file again.
   Clients resume syncing where they stopped.

If the database is lost and there is no dump, migrate an empty database and run
`capsule-server rebuild --apply` over the blob directory. It restores every asset row from the
owner ledgers. Accounts, published device directories, album membership and share links are in
the database only, and a rebuild does not restore them.

Rehearse this before you trust real data to it, for example by checking with `capsule restore` that
every original comes back byte for byte. `capsule-e2e/tests/backup_restore_drill.rs` runs the same
steps against containers (`mise run test-rust-containers`).
