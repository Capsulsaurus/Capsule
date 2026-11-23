# ADR-0023 — The server image is distroless glibc, multi-arch, non-root and read-only

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Self Hosting — Container image](../capsule-docs/src/content/docs/guides/self-hosting.md#container-image)
- **Issues:** #1246, #1214

## Context

Deployments are expected to be containerized (Podman or Kubernetes), and no image exists for the
rebuilt server. The only Containerfile in the tree is the retired Salvo server's
(`legacy-review/server-salvo/Containerfile`), which already used `gcr.io/distroless/cc-debian12`
but ran as root and built a binary that no longer exists. Dependabot's `docker` entry was removed
because it watched that absent file (`.github/dependabot.yml`).

What the image has to hold follows from the server:

- The release builds the server for `x86_64-unknown-linux-gnu` only (`.github/workflows/release.yml`,
  the `build` matrix), so the shipped binary links glibc.
- The server sets no global allocator and uses the system allocator. The allocator policy keeps it
  until a measured A/B shows a win ([performance — Allocator](../capsule-docs/src/content/docs/development/performance.md#allocator)).
- The server never terminates TLS. Its outbound TLS is rustls with bundled roots, so it needs no
  system TLS library or certificate store.
- It writes only under `BLOB_ROOT`: no temp files, no caches elsewhere.
- It logs to stderr and shuts down gracefully on `SIGTERM` (`capsule-server/src/cli.rs`).
- One `serve` process owns a blob root, because upload claims are in-process
  ([upload](../capsule-docs/src/content/docs/design/transfer/upload.md)).
- It has no health route: every route is versioned API, a share or drop page, or `.well-known`.

## Decision

The server ships as one OCI image on `gcr.io/distroless/cc-debian12:nonroot`, pinned by digest,
holding `capsule-server` and `capsule-server-migration` from the same build. It is multi-arch
(`linux/amd64`, `linux/arm64`), each architecture built natively for its `*-unknown-linux-gnu`
target. It runs as UID 65532 with a read-only root filesystem, no capabilities, and the blob
directory as its only writable volume. The server gains unversioned `/livez` and `/readyz` routes
and a `capsule-server healthcheck` subcommand, because the image has no shell or HTTP client. One
replica serves a blob root, updated by stop-then-start.

Compared:

- **scratch with a static musl binary.** The smallest image, but musl's allocator is markedly
  slower than glibc's under a multi-threaded Tokio runtime, so it would force a replacement
  allocator (mimalloc or jemalloc) before any measurement, which the allocator policy forbids. It
  also adds a second Linux target to build and test beside the released glibc one, and scratch
  has no `/etc/passwd` entry or CA bundle unless the build assembles them.
- **Alpine.** musl as above, plus a shell and a package manager the server never uses.
- **debian-slim.** glibc, but with a shell, apt and a full userland: a larger attack surface and a
  steady stream of scanner findings for packages the server never loads.
- **A native binary only.** Remains supported (the release tarball, under systemd), but leaves
  every operator to assemble the user, read-only filesystem, migration ordering and health checks
  themselves. The image is the reference deployment; the native binary follows the same shape.

## Consequences

- The release matrix gains `aarch64-unknown-linux-gnu` on a native arm64 runner rather than QEMU
  emulation, which is slow enough to dominate a release.
- glibc pins a floor: the binary must be built against a glibc no newer than Debian 12's, so the
  image build compiles in a Debian 12 builder.
- Debugging in the running container has no shell. Operators attach an ephemeral debug container
  (`kubectl debug`, `podman run --pid=container:…`) instead.
- Dependabot regains a `docker` entry, in the change that adds the Containerfile, to move the base
  digest.
- A rolling update is unsupported until multi-replica uploads are decided; the Kubernetes example
  uses `Recreate`.
- The health routes become part of the server's surface and are exempt from protocol-version
  negotiation.
