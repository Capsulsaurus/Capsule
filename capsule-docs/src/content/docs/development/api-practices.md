---
title: API Practices
description: Contract and layering rules for the Kynos REST/OpenAPI server
status: draft
---

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

Kynos types stop at the endpoint layer. Domain services are callable with ordinary Rust values and
deterministic mocks. Database models never become wire DTOs.

## Public Contract

- REST/OpenAPI is the only public server transport; Kynos owns runtime, routing, middleware, OpenAPI
  emission, limits, shutdown, and runtime observability.
- The checked-in [OpenAPI 3.2 document](/design/api-surfaces/#the-document-is-32-and-its-version-is-pinned)
  is generated deterministically without live infrastructure; Spargen generates the client from it.
- Every error carries a stable `error.*` code; the English detail is diagnostic and clients localize
  the code.
- Protocol version headers are present on successful and failed responses.
- Binary bodies stream with backpressure and cancellation; uploads and range downloads never buffer a
  whole blob.

## E2EE Boundary

The server may receive ciphertext, hashes, declared ciphertext sizes, blob roles, owner/album/file
identifiers, protocol and crypto-suite identifiers, lifecycle action, chain links, and the key-free
manifest envelope. It MUST never receive, persist, log, or return plaintext filenames, capture times,
dimensions, GPS, EXIF, tags, faces, LQIP, or decoded media.

Structural envelope validation runs before writes; client-side cryptographic verification remains
authoritative.

## Storage and State

- Capsule owns the blob-store contract; backends implement one narrow trait.
- Capsule owns the E2EE-aware resumable-upload state machine; no external transfer server owns
  reservation, provenance, bundle visibility, or finalization.
- `AuthStateStore` and `UploadSessionStore` are distinct typed ports with Valkey and in-memory
  adapters passing the same conformance suites. Valkey is
  [required](/design/filesystem/server/#required-services); the in-memory adapter is a test double,
  not a deployment choice.

## Logging

Every critical process must be reconstructable from its logs. Use `tracing` with structured fields
(never the `log` facade, never values formatted into the message) and `#[instrument]` hot-path
functions so spans carry the identifiers an investigation needs.

- **ERROR** unexpected failures; **WARN** recoverable or unusual; **INFO** domain events
  (reservation, finalization, lifecycle transitions, auth decisions); **DEBUG** critical-process
  flow; **TRACE** per-request and per-chunk detail. Use DEBUG and TRACE aggressively.
- Never log plaintext or anything on the E2EE-forbidden list, credentials or key material, tokens,
  production PII, or blob contents. Errors carry the `error.*` code, never sensitive detail.

## Tests Before Logic

Define DTOs, ports, errors, state transitions, and negative cases before implementation; mock every
external dependency. No route is complete without contract tests for invalid schemas, authorization
failure, cancellation, backend failure, protocol mismatch, and unknown future fields/statuses.
