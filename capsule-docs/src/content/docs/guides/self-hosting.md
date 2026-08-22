---
title: Self-Hosting
description: Get control of your data
---

Capsule was meant to be self-hosted from the start. This guide covers what the `capsule-api` server needs to run: the services it depends on, the environment it reads, and the maintenance jobs an operator schedules.

Skip to [Deploying](#deploying) for the runnable instructions.

## Prerequisites

Unlike other open-source projects, Capsule makes some assumptions about your environment and provides very specific instructions for setup. Assume some technical knowledge; a container runtime and a shell are the baseline.

## Hardware

*For a single-node deployment.*

- Operating System: Some modern GNU/Linux distribution capable of running containers (Docker or Podman)
- CPU: Most modern x86 and arm64 chips should work. AES-NI should be enabled. Intel QAT is not necessary for TLS (`rustls`).
- RAM:
  - Minimum: 2 GB
  - Recommended: 4 GB
- Storage: enough for your library plus headroom for in-flight uploads. Chunks are staged under `UPLOAD_DIR` before being renamed into the content-addressed blob tree, and the whole tree must live on **one** filesystem so that finalization renames are atomic.

## Software

Since Capsule uses container technologies for both development and production, the specific software requirements are less important other than to ensure potential compatibility issues are isolated for the most common, popular target (i.e., some sort of Linux distribution with glibc). For beginners, we recommend installing the newest Ubuntu LTS server (although other distributions like Rocky Linux are officially tested on). Docker/Podman installation is easiest.

### Components

The Capsule API is written almost entirely in Rust. It builds as **one binary** whose surfaces are selected by cargo features (`auth`, `upload`, `media`, `sync`, `openapi`; `full` — the default — enables all of them), so a small deployment runs a single process and a large one can build feature-scoped images:

- **Auth** (`auth`): REST surfaces for sessions, passkeys, TOTP, OIDC, the device directory, and master-key escrow.
- **Upload** (`upload`): the [TUS-style resumable upload protocol](/design/import/upload-protocol/) plus the lifecycle-write (`ops`) surface and quota accounting.
- **Media** (`media`): REST serving of ciphertext blobs (with HTTP `Range`), share links, guest drops, storage verification, and custody receipts.
- **Sync** (`sync`): the `capsule.sync.v1` gRPC feed and federation pull. It is mounted on the same router as the REST surfaces and wrapped in `tonic_web::GrpcWebLayer`, so the same service answers browser gRPC-web calls — **no Envoy or Istio sidecar is required**.

Two additional one-shot binaries ship for operators (see [Maintenance jobs](#maintenance-jobs)): `gc` and `scrub`.

The server is **key-free**: it never holds album keys and cannot read content. Library queries (timeline, albums, search) have no server surface at all — clients answer them locally against a synced `library.sqlite`. See [API Surfaces](/design/api-surfaces/).

### External dependencies

Exactly two:

- [PostgreSQL](https://www.postgresql.org/): the durable index — assets, albums, memberships, sessions, upload rows, quota.
- [Valkey](https://valkey.io/): upload-session state, caching, and other ephemeral data. It is required, not optional.

Blob bytes are **not** in object storage. They live on the filesystem under `UPLOAD_DIR`, content-addressed as `blobs/{hash}.bin` — the layout is owned by [Filesystem — Server](/design/filesystem/server/). There is no MinIO/S3 dependency: the `gc` and `scrub` binaries read the same directory the server writes.

## Configuration

The server reads its configuration from the environment (a `.env` file beside the binary is loaded automatically). `capsule-api/.env.example` is the starting point.

**Required** — startup fails without these:

| Variable | Meaning |
| --- | --- |
| `DATABASE_URL` | PostgreSQL connection string |
| `VALKEY_URL` | Valkey connection string (e.g. `redis://127.0.0.1:6379`) |
| `JWT_ED25519_DER` | Base64 of a PKCS#8 DER Ed25519 private key — the server's operational signing key. Generate with `mise run keygen` (or `cargo run -p capsule-api --bin keygen`), which mints it through the same module that parses it at boot. |

**Optional, with defaults** — everything else. The notable ones:

| Variable | Default | Meaning |
| --- | --- | --- |
| `UPLOAD_DIR` | `./uploads` | Root of the blob store and upload staging area |
| `SERVER_HOST` | `0.0.0.0` | Bind address |
| `SERVER_PORT` | `3000` | Bind port |
| `SERVER_DOMAIN` | `localhost` | Public domain; set this to your real hostname |
| `ALLOWED_ORIGINS` | *empty in release builds* | Comma-separated CORS allowlist. Release builds default to allowing **nothing**, so a browser client needs this set explicitly. (Debug builds default to `*`.) |
| `LOG_LEVEL` | `INFO` in release, `TRACE` in debug | `tracing` level filter; release builds log JSON |
| `MAX_FILE_SIZE` | compiled-in constant | Upload/serve size ceiling in bytes |
| `TOTP_ISSUER` | compiled-in constant | Issuer string shown in authenticator apps |
| `JWT_ACCESS_TOKEN_DURATION_SECONDS`, `JWT_REFRESH_TOKEN_DURATION_SECONDS` | compiled-in constants | Token lifetimes |

**Optional, derived if absent** — these are secrets the server will deterministically derive from `JWT_ED25519_DER` via HKDF, so they are stable across restarts without operator action. Set them only if you want them independent of the signing key (for example, to rotate one without invalidating the other):

| Variable | Meaning |
| --- | --- |
| `ATTESTATION_KEY_SEED` | Base64, 32 or 64 bytes. The hybrid (Ed25519 ‖ ML-DSA-65) seed that signs custody receipts and storage attestations. |
| `ATTESTATION_KEY_HISTORY` | Base64 of the well-known document's `keys` array, retaining retired public keys so pre-rotation receipts still verify. No default — absent means "no rotation yet". |
| `SYNC_CURSOR_MAC_KEY` | Base64, exactly 32 bytes. Server-only HMAC key for the opaque sync cursor. |

## Deploying

### Database migrations

Migrations run automatically **only in debug builds**. A release deployment must apply them itself before (or as part of) rollout — the server will not create or upgrade its own schema in production. The migrations live in the `capsule-api-migration` crate; as of 2026-08-21 that crate is a library with no bundled migrator binary, so applying them out of band is an operator step with no first-party command yet. This is a known gap.

### Containers

`capsule-api/Containerfile` builds a release image (a `rust:bookworm` build stage into a distroless runtime) exposing port 3000. It is the same image shape used in development.

For the two backing services, `capsule-api/compose.yaml` brings up PostgreSQL and Valkey:

```bash
podman compose -f capsule-api/compose.yaml up -d   # or: docker compose -f …
```

### One-click installer

*Not yet documented (2026-08-21).* No first-party installer script exists in this repository yet. Until one does, follow the container instructions above.

### Kubernetes

*Not yet documented (2026-08-21).* No Helm chart or manifests ship in this repository, and no Kubernetes deployment is tested in CI. The server is a single stateless process plus PostgreSQL, Valkey, and one `ReadWriteOnce` volume for `UPLOAD_DIR`, so it is straightforward to express — but nothing here is validated, and this guide will not pretend otherwise. Note that the blob tree must be a single filesystem for finalization renames to be atomic, which rules out fanning `UPLOAD_DIR` across volumes.

## Maintenance jobs

Both are one-shot binaries an operator crons; neither is a daemon. Both read `DATABASE_URL` and `UPLOAD_DIR` exactly the way the server does.

- **`gc`** — the retention purge (hard-purge soft-deleted assets whose signed `retention_until` has elapsed) plus the two-phase refcount mark-and-sweep that reclaims unreferenced blob bytes past the GC grace window. `--dry-run` reports what would be reclaimed without deleting anything.

  ```bash
  cargo run --release -p capsule-api --bin gc -- --dry-run
  ```

- **`scrub`** — the read-only integrity scrub: it verifies the Postgres index against the blob store (row⇄blob presence, custody-chain agreement, mirrored-fact agreement, debris inventory) and **exits non-zero when any finding is present**, which is the signal to alert on. `--deep` adds a per-blob byte re-hash. It mutates nothing by design.

  ```bash
  cargo run --release -p capsule-api --bin scrub -- --deep
  ```

See [Filesystem — Maintenance](/design/filesystem/maintenance/) for what each check means.
