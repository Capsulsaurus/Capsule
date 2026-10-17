---
title: Self Hosting
description: Current self-hosting status and planned deployment dependencies
status: draft
---

The previous server has been removed from the active workspace while its Kynos replacement is being
designed. There is currently no supported Capsule server deployment. Sources under
`legacy-review/` are not deployable.

## Planned Profile

The supported server will expose one REST/OpenAPI API and require:

- PostgreSQL for the authoritative key-free index.
- A Capsule-owned filesystem blob store for opaque content-addressed ciphertext.
- Valkey for volatile session state — upload sessions and auth sessions. It is required at every
  size of deployment, not a scaling option; see [Valkey is required](#valkey-is-required) below.
- A TLS-terminating ingress or Kynos-native TLS configuration appropriate to the deployment.

Clients perform media processing, metadata extraction, derivative generation, encryption, signing,
and cryptographic verification. The server never decodes uploaded media.

Deployment instructions will be published only after the Kynos server, migrations, storage
verification, backup/restore, readiness, graceful shutdown, and upgrade tests are complete.

## Planned deployment profile — inherited operator facts

Nothing below is a supported deployment instruction. These are durable operator-facing constraints
established by the quarantined server that the Kynos design is expected to inherit (or to
deliberately overturn). They are recorded here because this guide is the only place they exist.

### Blob tree must be one filesystem

The whole blob tree must live on a **single** filesystem, so that finalization renames chunks staged
under the upload directory into their [sharded content address](/design/filesystem/server/#blob-store-layout)
under `blobs/` atomically. Any POSIX filesystem meeting that holds; no particular filesystem is
certified. It rules out fanning the blob root across volumes —
including across Kubernetes PersistentVolumes. See
[Filesystem — Server](/design/filesystem/server/) and
[Filesystem — Maintenance](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery).

### Key derivation couples three identities to one signing key

`SYNC_CURSOR_MAC_KEY` (the server-only HMAC key for the opaque sync cursor) and
`ATTESTATION_KEY_SEED` (the hybrid Ed25519 ‖ ML-DSA-65 seed that signs custody receipts and storage
attestations) are HKDF-derived from `JWT_ED25519_DER` when they are not set explicitly. That makes
them stable across restarts with no operator action — but it also means **rotating the operational
signing key silently rotates the sync-cursor MAC and the attestation identity**, invalidating
outstanding cursors and breaking verification of pre-rotation receipts unless the retired public
keys are retained (`ATTESTATION_KEY_HISTORY`). Operators who want independent rotation must set the
derived secrets explicitly. The replacement server must either keep this derivation and document the
coupling loudly, or decouple the three keys.

### CORS defaults to nothing in release builds

`ALLOWED_ORIGINS` (the comma-separated CORS allowlist) defaults to allowing **nothing** in release
builds — debug builds default to `*`. A browser client therefore does not work against a release
deployment until the operator sets it explicitly.

### Migrations are a known gap

Migrations run automatically **only in debug builds**. A release deployment must apply them itself
before or as part of rollout; the server does not create or upgrade its own schema in production.
The migration crate is a library and **no migrator binary ships**, so applying them out of band has
no first-party command. This is a named open gap the Kynos work must close.

### Two one-shot operator binaries

Neither is a daemon; both are cronned by an operator and read the same database and blob root the
server does.

- **`gc`** — the retention purge (hard-purge of soft-deleted assets whose signed retention deadline
  has elapsed) plus the two-phase refcount mark-and-sweep that reclaims unreferenced blob bytes past
  the GC grace window. `--dry-run` reports what would be reclaimed without deleting anything.
- **`scrub`** — the read-only integrity scrub: it verifies the index against the blob store
  (row⇄blob presence, custody-chain agreement, mirrored-fact agreement, debris inventory) and
  **exits non-zero when any finding is present**, which is the signal to alert on. `--deep` adds a
  per-blob byte re-hash. It mutates nothing by design.

See [Filesystem — Maintenance](/design/filesystem/maintenance/) for what each check means.

### Valkey is required

Settled, and the operator-facing consequence is blunt: a single-node deployment runs Valkey too.
`VALKEY_URL` is a required startup variable — the server refuses to boot without it — and Valkey
holds upload-session and auth-session records, not a cache whose absence merely costs speed. The
in-memory adapter is a test double and is never a deployment mode. The alternative, a Postgres-backed
session store so a small deployment could skip Valkey, was rejected: it means emulating TTL and
expiry in SQL, for a second implementation of a contract the server already has exactly one of. The
earlier profile text here claimed Valkey was needed only for a measured high-concurrency profile;
that was the outlier, and the code never agreed with it. The full split is owned by
[Filesystem — Server: Required Services](/design/filesystem/server/#required-services).
