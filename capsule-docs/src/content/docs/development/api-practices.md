---
title: API Practices
description: Contract and layering rules for the Kynos REST/OpenAPI server
---

The previous server implementation is quarantined. New server code must follow this contract before
any route is reactivated.

## Layering

```text
Kynos endpoint and OpenAPI declaration
              ↓
Capsule request/response DTO mapping
              ↓
Framework-neutral application service
              ↓
Typed repository or backend port
              ↓
Postgres, Valkey, or Capsule BlobStore adapter
```

Kynos types stop at the endpoint layer. Domain services must be callable with ordinary Rust values
and deterministic mocks. Database models do not become wire DTOs.

## Public Contract

- REST/OpenAPI is the only public server transport.
- Kynos owns HTTP runtime startup, routing, middleware composition, OpenAPI emission, request
  context, limits, graceful shutdown, and runtime observability.
- A checked-in OpenAPI 3.1 document is generated deterministically without live infrastructure.
- Spargen generates the Rust client and runs support and compatibility checks in CI.
- Every error has a stable `error.*` code. English detail remains diagnostic; clients localize the
  code from the canonical catalogs.
- Protocol version headers and errors are present on successful and failed responses.
- Binary bodies stream with backpressure and cancellation. Uploads and range downloads never buffer
  a complete blob.

## E2EE Boundary

The server may receive ciphertext, hashes, declared ciphertext sizes, blob roles, owner/album/file
identifiers, protocol and crypto-suite identifiers, lifecycle action, chain links, and the key-free
manifest envelope. It must never receive, persist, log, or return plaintext filenames, capture
times, dimensions, GPS, EXIF, tags, faces, LQIP, or decoded media.

Structural envelope validation runs before writes. Client-side cryptographic verification remains
authoritative.

## Storage and State

- Capsule owns the blob-store contract and implementation. The filesystem backend is first; other
  backends implement the same narrow trait.
- Capsule owns the E2EE-aware resumable-upload state machine. Standard resumable HTTP behavior may
  inform it, but no external transfer server owns asset reservation, provenance, bundle visibility,
  or finalization.
- `AuthStateStore` and `UploadSessionStore` are distinct typed ports. Postgres, Valkey, and in-memory
  adapters run the same domain-specific conformance suites.

## Tests Before Logic

Define DTOs, service ports, errors, state transitions, and negative cases before implementation.
Mock every external dependency. Hot paths emit structured spans and timing metrics. No route is
complete without contract tests for invalid schemas, authorization failure, cancellation, backend
failure, protocol mismatch, and unknown future fields/statuses.
