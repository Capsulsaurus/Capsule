---
title: API Surfaces
description: The Kynos REST/OpenAPI server surface and its negotiation and rejection contracts
status: draft
---

Capsule exposes one public server transport: **Kynos REST with a checked-in OpenAPI 3.2
contract**. This document owns the surface-to-module map, the document model that contract is
served under, and the HTTP carriage of the
[universal handshake](/design/threat-model/validation/#protocol-and-capability-negotiation).
Handshake rules remain owned by Threat Model — Validation, and stable error-code identity by
[Internationalization](/design/i18n/#server-error-codes).

How these surfaces are published as developer reference — the artifact each is generated from and
the gate that keeps it current — is [Developer Documentation](/design/developer-docs/).

## Surface ↔ Transport Map

| Surface | Transport | Planned module | Owner doc |
| --- | --- | --- | --- |
| Authentication (sessions, TOTP, OIDC) | REST | `capsule-server::auth` | [Authentication](/design/authentication/) |
| Resumable upload (`POST /v1/upload`, then `HEAD/PATCH /v1/upload/{id}`) | REST | `capsule-server::upload` | [Upload Protocol](/design/import/upload-protocol/) |
| Lifecycle writes (`POST /v1/albums/{album_id}/ops`) | REST | `capsule-server::upload::ops` | [Authorization](/design/authorization/#the-lifecycle-write-surface) |
| Blob fetch (`GET /v1/blob/{hash}`, HTTP `Range`) | REST | `capsule-server::blob` | [Download & Sync](/design/import/download-sync/) |
| Sync feed (change discovery after a cursor) | REST | `capsule-server::sync` | [Download & Sync](/design/import/download-sync/) |
| Federation pull | REST | `capsule-server::federation` | [Federation](/design/federation/) |
| Share serving (`/s/{opaque_id}`) | REST | `capsule-server::share` | [Share Links](/design/share-links/) |
| Guest drops (`POST /d/{opaque_id}`, inbox, adoption) | REST | `capsule-server::drop` | [Web Upload](/design/web-upload/) |
| Storage verification (`POST /v1/storage/verify`) | REST | `capsule-server::blob` | [Storage Verification](/design/import/storage-verification/) |
| Device enrollment (`/auth/devices/enroll…`) | REST | `capsule-server::auth::devices` | [Device Enrollment](/design/device-enrollment/) |
| Version/handshake | REST | `capsule-server` | [Threat Model — Validation](/design/threat-model/validation/) |
| Library queries (timeline, albums, search) | none — client-side over `library.sqlite` | `capsule-core::library` + `db` | [Organization](/design/organization/#system--smart-albums-views) |

Blob bytes use ranged HTTP. The sync feed carries only small opaque envelopes, encrypted metadata,
and blob references; clients fetch content separately with `Range`. Rich content queries have no
server surface because the key-free server cannot evaluate them.

**Status note (web client).** The browser's authenticated read path is a key-free projection of the
sync feed (ids, membership, blob addresses); the Worker-hosted WASM decode/verify boundary that
would fill decrypted titles/thumbnails is **post-v1** (decision 2026-07-12). v1's web surfaces are
the guest drop and the share-link viewer.

## Why REST/OpenAPI Only

The checked-in OpenAPI **3.2** document is the public contract and the input to Spargen. Kynos owns
routing, middleware composition, and deterministic schema emission. Capsule owns authentication,
protocol headers, error bodies, encrypted-upload state, blob storage ports, and business rules.

### The Document Is 3.2, and Its Version Is Pinned

3.2 is not a floor the document may drift above; it is the version the document declares.
`capsule-server::openapi()` calls `router().openapi_as(SpecVersion::V3_2)` rather than the plain
`router().openapi()`, because the plain emitter returns the *lowest* version that expresses the API
without loss — a sound default for a document produced on demand, and the wrong one for a contract
that is committed to the repository and generated from. Left to follow the API, the committed
contract would flip 3.1 → 3.2 the day the first streamed response landed, churning the schema gate
and regenerating the SDK for a change nobody asked for.

Enabling Kynos's `openapi32` feature is a *different* thing and does not by itself produce a 3.2
document; why the emitted version is deliberately not keyed on a feature flag belongs to the Kynos
row in [Dependencies](/design/dependencies/#rust). What matters here is the consequence:
`openapi_as` targets rather than downgrades, so a construct 3.2 cannot express is an error naming
what blocks it, never a document with operations quietly missing.

### What Is Generated, and What Is Not

The line is **parsing and serialization versus orchestration**, and with Spargen's two gaps closed
it now falls in exactly one place.

*Generated from the contract*: every request and response body, every typed parameter, and the
byte-serving endpoints. The blob-fetch and asset-serve tree — `GET /v1/blob/{hash}` with `Range`, and
the derivative reads beside it — is back in the generated client, because textual and binary
response decoding and typed parameter serialization both work now. A hand-written byte path is no
longer justified by a generator limitation, and adding one back would be a second parser for a
surface the contract already describes.

*Hand-written*: the resumable upload state machine (slice `S-D1`) — chunk scheduling, offset
resumption after an interruption, retry laddering, and the connection-class budget. None of that is
parsing. It is orchestration **over** the generated calls, driven by conditions an OpenAPI document
cannot express, which is why it was never a generator gap and does not close with one. This is the
existing contract with the gaps removed, not a new one; the Spargen pin and its history are the
codegen row in [Dependencies](/design/dependencies/#rust).

One transport keeps negotiation, observability, error handling, streaming, cancellation, and test
harnesses consistent. GraphQL and gRPC are retired architecture, not compatibility surfaces.
Review-only implementations under `legacy-review/` may inform contract tests but cannot be mounted
or restored directly.

The `capsule.sync.v1` gRPC feed and the gRPC-web framing that served it to the browser retired with
the Salvo server: sync and federation pull are REST operations on the same OpenAPI contract, with
the signed manifest still travelling as opaque canonical CBOR (never re-modelled as wire fields —
re-encoding would detach it from its signatures).

**The browser followed** (slice `S-C60`). `capsule-web` spoke gRPC-web through a hand-rolled
Protobuf codec — 410 lines of varint and length-delimited framing, written to avoid pulling a
`protobuf-es`/`connect-web` toolchain in to read a handful of fields — and now issues one
`GET /v1/sync` and parses JSON. The store's client rules are untouched: forward-version rejection
and per-album anti-rewind were always behind a transport seam, which is what a seam is for.

Two things the browser can no longer see, and both are the REST feed being *stricter*: a blob's
MIME type, which was plaintext metadata about an encrypted blob and is simply not a field any
more, and an "unspecified" change kind, which existed only because a proto3 enum defaults to zero
when a field is absent.

## Legacy: GraphQL (removed)

A now-deleted crate exposed an `async-graphql` schema at `/v1/library`. It predated the
E2EE key-free server model and was **removed** in slice S-G1 (repo-root `SLICES.md`); no GraphQL
surface exists. It was never evolving, for reasons that also foreclose reviving it:

- Its resolvers presumed a server that can read content (people, faces, smart tags, memories
  server-side) — structurally impossible under the [threat model](/design/threat-model/); the
  key-free replacements are client-side ML ([AI/ML](/design/ai/)) and client-side views
  ([Organization](/design/organization/#system--smart-albums-views)).
- The generated SDK is OpenAPI-derived and cannot drive GraphQL, so the surface was never consumable
  by the client stack the design commits to.
- The query role it aimed at is served client-side over `library.sqlite` (above), fed by the sync
  feed — the parity precondition that unblocked retirement.

## Negotiation Across Transports

Every public route applies the same headers:

| Header | Direction |
| --- | --- |
| `X-Capsule-Protocol` | request |
| `X-Capsule-Crypto-Suite` | request for writes |
| `X-Capsule-Sidecar-Schema` | request |
| `X-Capsule-Protocol-Min` | response |
| `X-Capsule-Protocol-Max` | response |
| `X-Capsule-Min-Client-Build` | response |

Credentials use `Authorization: Bearer`. Session access tokens and federation capabilities are
different token types verified by their owning modules, even though both use the standard HTTP
carriage.

## Rejection Mapping

REST status is coarse; the stable `error.*` code in the `ApiError` body is the precise discriminator.
Clients switch on the code, never on status alone.

| Rejection class | HTTP status |
| --- | --- |
| Structural (bad envelope, unknown enum, sizes) | `400` |
| Unauthenticated or expired token | `401` |
| Unauthorized (capability, quota-hard, suspension) | `403` |
| Not found, including indistinguishable-404 surfaces | `404` |
| Stale state (chain, cursor, directory regression) | `409` |
| Payload too large | `413` |
| Unsupported chunk media type | `415` |
| Protocol outside `[Min, Max]` | `426` |
| Rate limited | `429` |

## Validation

- Drive every fail-closed handshake rule through representative routes in each module and assert
  the same headers, status, and `error.*` code.
- Generate the OpenAPI contract twice and assert byte-identical output, and assert the emitted
  document declares OpenAPI **3.2** — a contract that silently reverted to 3.1 would still be
  valid, still generate, and no longer be the committed decision. Generate the Spargen client from
  the checked-in contract and fail CI on drift, byte-serving operations included.
- Exercise streaming upload and ranged download with cancellation, backpressure, retry, and body
  limits without live infrastructure.
- Present valid and invalid federation capabilities through REST and assert compartmentalized,
  indistinguishable rejection behavior.
