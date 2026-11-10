---
title: API Practices
description: Contract and layering rules for the Kynos REST/OpenAPI server
status: draft
---

## Layering

```mermaid title="A request crosses four layers before it reaches a backend"
flowchart TB
  endpoint[Kynos endpoint and<br/>OpenAPI declaration]:::server
  dto[Capsule request/response<br/>DTO mapping]:::server
  service[Framework-neutral<br/>application service]:::accent
  port[Typed repository<br/>or backend port]:::server
  backend[(Postgres or<br/>Capsule BlobStore adapter)]:::store
  endpoint --> dto --> service --> port --> backend
```

Kynos types stop at the endpoint layer. Domain services are callable with ordinary Rust values and
deterministic mocks. Database models never become wire DTOs.

## Public Contract

- REST/OpenAPI is the only public server transport. Kynos owns runtime, routing, middleware, OpenAPI
  emission, limits, shutdown and runtime observability.
- The checked-in OpenAPI 3.2 document ([](/design/api-surfaces/#key-design-decisions)) is generated
  deterministically without live infrastructure, and Spargen generates the client from it.
- Every error carries a stable `error.*` code. The English detail is diagnostic; clients localize the
  code.
- Protocol version headers are present on successful and failed responses.
- Binary bodies stream with backpressure and cancellation. Uploads and range downloads never buffer a
  whole blob.

The headers, error contract and status mapping themselves are specified in
[](/reference/api/conventions/).

## E2EE Boundary

The server may receive ciphertext, hashes, declared ciphertext sizes, blob roles, owner/album/file
identifiers, protocol and crypto-suite identifiers, lifecycle action, chain links and the key-free
manifest envelope. It MUST never receive, persist, log or return plaintext filenames, capture times,
dimensions, GPS, EXIF, tags, faces, LQIP or decoded media.

Structural envelope validation runs before writes. Client-side cryptographic verification remains
authoritative.

## Storage and State

- Capsule owns the blob-store contract, and backends implement one narrow trait.
- Capsule owns the E2EE-aware resumable-upload state machine. No external transfer server owns
  reservation, provenance, bundle visibility or finalization.
- `AuthStateStore` and `UploadSessionStore` are distinct typed ports with PostgreSQL and in-memory
  adapters passing the same conformance suites. PostgreSQL is
  required ([](/design/filesystem/server/#required-services)); the in-memory adapter is a test double,
  not a deployment choice.

## Logging

Every critical process must be reconstructable from its logs. Use
[`tracing`](https://github.com/tokio-rs/tracing) with structured fields, never the `log` facade and
never values formatted into the message. `#[instrument]` hot-path functions so spans carry the
identifiers an investigation needs.

| Level | Use |
| --- | --- |
| ERROR | Unexpected failures |
| WARN | Recoverable or unusual conditions |
| INFO | Domain events: reservation, finalization, lifecycle transitions, auth decisions |
| DEBUG | Critical-process flow (use aggressively) |
| TRACE | Per-request and per-chunk detail (use aggressively) |

Never log plaintext or anything on the E2EE-forbidden list, credentials or key material, tokens,
production PII, or blob contents. Errors carry the `error.*` code, never sensitive detail.

## Tests Before Logic

Define DTOs, ports, errors, state transitions and negative cases before implementation, and mock
every external dependency. No route is complete without contract tests for invalid schemas,
authorization failure, cancellation, backend failure, protocol mismatch, and unknown future fields
and statuses.
