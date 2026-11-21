---
title: Self Hosting
description: Running capsule-server on PostgreSQL and a filesystem blob store
status: draft
---

No release has been cut, so you build the server from source. The server stores only ciphertext and
never decodes media; clients do all processing and encryption.

## What you need

- **PostgreSQL 18**: the durable index (accounts, assets, quota) and the server's expiring state
  (sign-in sessions, upload sessions, rate counters). It is the only database the server uses, and
  it must run with its durability settings intact ([](#postgresql)).
- **A blob directory** (`BLOB_ROOT`) on **one POSIX filesystem** that honours `fsync` and shares file
  locks. It holds the blobs and, under `ledger/`, each owner's signed ledger, from which the index
  can be rebuilt ([](#blob-directory)).
- **A TLS-terminating reverse proxy** in front of the server that passes compression through
  untouched ([](#reverse-proxy)).
- **A synchronized clock** on the host, kept by NTP ([](#clock)).

### PostgreSQL

The server answers a write once its row commits, so a commit PostgreSQL acknowledges and then loses
can bring back a revoked session or accept a second-factor code twice. Keep:

- `fsync = on` and `full_page_writes = on`, the defaults;
- `synchronous_commit` at `on`, `local`, `remote_write` or `remote_apply`, never `off`;
- the data directory on storage that honours a flush. A disk write cache with no power-loss
  protection, or a virtual disk whose host ignores flushes, loses acknowledged commits on a power
  cut ([](/design/filesystem/server/)).

PostgreSQL may live on a different filesystem, disk or host from `BLOB_ROOT`.

### Blob directory

`BLOB_ROOT` is a contract with the filesystem
([](/design/filesystem/server/#blob-root-filesystem-contract)), laid out as in
[](/design/filesystem/server/#blob-store-layout):

- **One filesystem.** Finalization is an atomic rename, so the whole tree, `incoming/` included,
  cannot span volumes, datasets or Kubernetes PersistentVolumes.
- **`fsync` is honoured**, on files and on directories, before the server acknowledges a write.
- **File locks are shared** by every process that writes the tree. `serve` and `purge --apply` lock
  each owner's `ledger/<owner>/.lock` before appending
  ([](/design/filesystem/server/#concurrent-appenders)).
- **One `serve` per blob directory.** Upload chunks and finalization are claimed inside the serving
  process ([](/design/transfer/upload/)), so two `serve` processes on one tree, on one host or on
  two, are not supported. `gc`, `purge` and `scrub` may run beside it; `rebuild --apply` needs it
  stopped ([](#maintenance)).

Supported:

- ext4, XFS and btrfs on local or block storage: a disk, RAID, LVM, iSCSI or a cloud block volume;
- ZFS with `sync=standard` (the default) or `sync=always`;
- NFSv4, or NFSv3 with its lock manager, exported `sync`;
- a Kubernetes PersistentVolume or a container volume backed by one of the above.

Not supported:

- a mount that disables write barriers, or a virtual disk whose host ignores flushes;
- ZFS with `sync=disabled`, which acknowledges `fsync` without writing;
- NFS exported `async`, which acknowledges writes the NFS server has not stored, or mounted
  `nolock`, `local_lock=flock` or `local_lock=all`, which keep locks to one host;
- SMB/CIFS shares;
- FUSE filesystems (sshfs, rclone mount, s3fs, gcsfuse) and any object-store mount;
- pooling and union filesystems that spread one tree across disks (mergerfs, Unraid user shares
  under `/mnt/user`): a rename between member disks is a copy, not an atomic rename;
- a container's own writable layer, and a macOS or Windows host directory bind-mounted into a
  container VM. Use a volume inside the VM.

On Linux, `flock` over NFS is emulated with byte-range locks, which the lock manager shares between
hosts. With locking disabled, `serve` and `purge --apply` on different hosts can interleave their
appends to a ledger.

- **Run a power-cut drill before trusting the hardware.** `serve` checks the filesystem type,
  atomic rename and lock sharing at boot, but no software check can see a disk, controller or
  hypervisor that acknowledges a flush it has not made. On the deployed hardware, upload while you
  cut power (not a clean shutdown), then boot and run `capsule-server scrub --deep`: every upload
  the server acknowledged must be intact
  ([](/design/filesystem/server/#blob-root-filesystem-contract)).
- **Leave free space.** The blob root keeps a free-space reserve, by default the larger of 2% of
  the volume and 4 GiB. Below it, new uploads are refused with `507` while uploads already on the
  server finish and deletes still work ([](/design/quota/#enforcement-points)). The reserve is
  planned and not built yet; until it is, a full disk refuses every write.
- **Prune quarantine deliberately.** `quarantine/` keeps blobs that failed a re-hash or could not
  be read until you delete them, except those since repaired. The planned
  `capsule-server quarantine prune` lists before it removes and removes nothing without `--apply`
  ([](/design/filesystem/server/#quarantine-retention)). `scrub` reports the quarantine's size, so
  watch it there.

### Reverse proxy

TLS terminates at the proxy, never at the server
([](/design/networking/#transport-security)). The server listens for plain HTTP and must be
reachable only from the proxy: on loopback, a private network or a pod network. Configure the
proxy to:

- accept request bodies of at least 16 MiB, the largest upload chunk;
- stream request bodies to the server rather than buffering them;
- disable TLS early data (0-RTT), or forward it with the `Early-Data: 1` header so the server can
  refuse a write;
- apply no total-duration timeout to a request body, and an idle timeout longer than the
  server's stall bound, so a slow but steady chunk is never cut;
- **pass compression through.** The server decides which responses to compress and never
  compresses `application/octet-stream` ([](/design/networking/#compression)). The proxy passes
  Capsule's upstream responses through as the server sent them: it MUST NOT gzip or otherwise
  compress a response, MUST NOT decompress one or add, change or strip `Content-Encoding`, and MUST forward
  the client's `Accept-Encoding` unchanged. In nginx, keep `gzip off;` for the Capsule site, which
  overrides a `gzip on;` inherited from the distribution's `nginx.conf`. In Caddy, add no `encode`
  directive; in Traefik, attach no `compress` middleware.

HTTP/2 and HTTP/3 on the client side of the proxy are optional; clients fall back to HTTP/1.1.
Kernel TLS (kTLS) and NIC TLS offload, where you want them, are proxy settings (in nginx,
`ssl_conf_command Options KTLS;` with `sendfile on;`). The server's own TLS stack is used only for
the connections it opens: to an OpenID Connect provider and to federated peers. Those trust the
bundled Mozilla roots plus any `OIDC_CA_BUNDLE`, not the host's certificate store.

### Clock

The server judges time by its own clock, and several rules depend on that clock being right. Keep
it synchronized with NTP (chrony, or systemd-timesyncd) on the host. A container reads the host's
clock, so synchronize the host. The offset SHOULD stay within one second, and MUST stay within 30
seconds. Past those bounds:

| Offset | What breaks |
| --- | --- |
| Over 30 s | Second-factor codes start failing: a code is accepted only within one 30 s step of the server's clock ([](/design/authentication/)) |
| Over 5 min | Federation stops in both directions: peers refuse this server's signed requests, hints and reports, and it refuses theirs ([](/design/federation/)) |
| Clock ahead | Trash is purged before its retention window ends, and a restore made in time is refused ([](/design/organization/#retention-window)). Sessions, share links and upload sessions expire early |
| Clock behind | Expired sessions, share links and counters stay valid for longer than configured |
| Gross | Clients refuse custody receipts whose `received_at` is implausible ([](/design/transfer/durability/)) |

Start `serve` only once the clock is synchronized. Under systemd, order it after
`time-sync.target` with `chrony-wait.service` or `systemd-time-wait-sync.service` enabled. A virtual
machine restored from a snapshot resumes with the snapshot's clock: let NTP step it before `serve`
starts.

The server checks its clock against the latest instant it has recorded
([](/design/filesystem/server/#server-clock)). At boot, a clock more than five minutes behind the
newest receipt or ledger entry makes `serve` refuse to start, naming both instants; at runtime, a
backward step of more than a second is logged at `ERROR`. If the recorded instant is the wrong one,
because the clock once jumped forward and a receipt was written before it was corrected, fix the
clock first, then set `CLOCK_ACKNOWLEDGE_RECORDED` to the exact instant the refusal named and start
`serve` once. The server logs the acknowledgement, records it, and checks only later instants from
then on; a value that does not match is refused.

**Planned.** The clock check and `CLOCK_ACKNOWLEDGE_RECORDED` are not built yet: today the server
does not detect or report clock skew itself, so the systemd ordering above is the only guard.

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

The binaries link glibc and need nothing else installed. Run them as an unprivileged user whose
only writable path is `BLOB_ROOT`, the same shape the container image below enforces.

## Container image

:::caution
No image is published yet, and the `/livez` and `/readyz` routes and the `healthcheck` subcommand
are not built yet. This section is the contract the image is built to.
:::

- **Base:** `gcr.io/distroless/cc-debian12:nonroot`, pinned by digest. It carries glibc, the
  GCC runtime, CA certificates and time-zone data, and no shell or package manager.
- **Architectures:** one multi-arch image for `linux/amd64` and `linux/arm64`, each built natively
  for its glibc target. Builds are not tuned to one CPU; hashing picks SHA-NI or the Armv8 crypto
  extensions at run time.
- **Contents:** `capsule-server` (the entrypoint, `serve` by default) and
  `capsule-server-migration`, from the same build.
- **User:** non-root, UID and GID 65532. The container needs no Linux capabilities: drop them all
  and set `no-new-privileges`.
- **Read-only root filesystem.** The server writes only under `BLOB_ROOT`, so mount the blob
  directory as the one writable volume, owned by UID 65532, and mount the root read-only.
- **Listener:** set `SERVER_HOST=0.0.0.0` (or `::`) inside the container; port 3000 is the default.
  Publish it only to the reverse proxy ([](#reverse-proxy)).
- **Logs** go to stderr; set `LOG_FORMAT=json` for a log collector.
- **Stopping:** `serve` shuts down gracefully on `SIGTERM`. An upload cut by a stop resumes from
  the server's committed ranges.
- **One replica.** Run exactly one `serve` per blob directory ([](#blob-directory)). On Kubernetes,
  use one replica with the `Recreate` update strategy: a rolling update starts the new server
  while the old one still serves.
- **Migrations** run from the same image tag before `serve` starts, as a Job, an init container or
  an `ExecStartPre=`: `capsule-server-migration up` ([](#migrate-then-start)).
- **Maintenance** commands run from the same image, as the same user, on the same volume, as
  CronJobs or timers ([](#maintenance)).
- **Health:** `/livez` answers a `GET` while the process serves and checks nothing else, for a
  liveness probe. `/readyz` answers only while PostgreSQL responds, the schema matches the
  binary and `BLOB_ROOT` is writable, for a readiness probe. Both sit outside `/v1`, so a probe
  does not depend on the protocol version, and neither needs to be reachable through the proxy.
  The image has no `curl`, so its own health check runs `capsule-server healthcheck`, which calls
  `/livez` on the local listener.
- **Clock:** the container reads the host's clock; synchronize the host ([](#clock)).

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

- **Guard the token key above all.** The database holds no bearer secret, so a leaked database
  or backup yields no session on its own; `JWT_ED25519_DER` is what turns such a leak into every
  live session ([](/design/authentication/#session-token)). Refresh-token reuse detection
  ([](/design/authentication/#token-lineage-and-reuse)) does not help against it, because a holder
  of the key mints tokens rather than replaying them. Keep it in your secret store, never in the
  database or in the backups of it, and rotate it after a suspected leak, which signs every device
  out.
- **A second-factor sealing key is planned.** The server will seal TOTP secrets at rest under a
  dedicated key, independent of the token key, so a database or backup leak yields no second
  factor and rotating the token key strands none
  ([](/design/authentication/#the-second-factor)). Its setting does not exist yet; when it lands,
  keep it apart from the database and its backups like the token key.
- **Open registration needs a default quota** (planned). Once quotas are configurable, `serve`
  refuses to start with password registration enabled and no default limit, and an operator who
  wants none sets it to `unlimited` explicitly, which is logged at `WARN` on every start
  ([](/design/quota/#thresholds-and-states)). Today every deployment runs unlimited.

`SYNC_CURSOR_MAC_KEY` is derived from `JWT_ED25519_DER` when unset, so rotating the token key also
invalidates outstanding sync cursors unless you set it explicitly. Federation is off unless
`FEDERATION_ENABLED=true` and `FEDERATION_URL` are set; leave it off.

Name your reverse proxy so the per-source rate limits see the client rather than the proxy.

| Variable | Meaning |
| --- | --- |
| `TRUSTED_PROXIES` | Comma-separated addresses and CIDR networks (`10.0.0.0/8`). A network of `/0` is refused, because it would believe every sender. |
| `TRUSTED_PROXY_HOPS` | Trusts that many forwarding hops nearest the server, whatever their address. |
| `TRUSTED_PROXY_HEADER` | The one forwarding field the server reads: `x-forwarded-for` (the default) or `forwarded` (RFC 7239). Any other value refuses to start. |

Unset, no forwarding header is believed. That is safe, but every client behind the proxy then
shares the proxy's budget, so one stranger guessing passwords slows sign-in for everyone
([](/design/authentication/#sign-in-limits)); the server logs a warning when it sees a forwarding
header while trusting no hop. The opposite mistake is worse: trusting a hop that does not append
the client's address lets every client pick its own rate-limit key. The field `TRUSTED_PROXY_HEADER` does not name is ignored whoever sends
it, so a client cannot choose its own rate-limit key by sending one that your proxy passes on. Name
the field your proxy appends to. With the default, have your proxy append the address it saw to
`X-Forwarded-For`, as nginx does with:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

Set `TRUSTED_PROXY_HEADER=forwarded` only if your proxy appends its own element to `Forwarded`
and does not write `X-Forwarded-For`.

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
