---
title: API Surfaces
description: The Kynos REST/OpenAPI server surface and its negotiation and rejection contracts
status: draft
---

Capsule exposes one public server transport: **Kynos REST with a checked-in OpenAPI 3.1
contract**. This document owns the surface-to-module map and the HTTP carriage of the
[universal handshake](/design/threat-model/validation/#protocol-and-capability-negotiation).
Handshake rules remain owned by Threat Model — Validation, and stable error-code identity by
[Internationalization](/design/i18n/#server-error-codes).

## Surface ↔ Transport Map

| Surface | Transport | Planned module | Owner doc |
| --- | --- | --- | --- |
| Authentication (sessions, passkeys, TOTP, OIDC) | REST | `capsule-api::auth` | [Authentication](/design/authentication/) |
| Resumable upload (`POST/HEAD/PATCH /upload`) | REST | `capsule-api::upload` | [Upload Protocol](/design/import/upload-protocol/) |
| Lifecycle writes (`POST /albums/{album_id}/ops`) | REST | `capsule-api::upload::ops` | [Authorization](/design/authorization/#the-lifecycle-write-surface) |
| Blob fetch (`GET /blob/{hash}`, HTTP `Range`) | REST | `capsule-api::blob` | [Download & Sync](/design/import/download-sync/) |
| Sync feed (change discovery after a cursor) | REST | `capsule-api::sync` | [Download & Sync](/design/import/download-sync/) |
| Federation pull | REST | `capsule-api::federation` | [Federation](/design/federation/) |
| Share serving (`/s/{opaque-id}`) | REST | `capsule-api::shares` | [Share Links](/design/share-links/) |
| Guest drops (`/u/{opaque-id}/drop`, inbox, adoption) | REST | `capsule-api::drops` | [Web Upload](/design/web-upload/) |
| Storage verification (`POST /storage/verify`) | REST | `capsule-api::blob` | [Storage Verification](/design/import/storage-verification/) |
| Device enrollment (`/auth/devices/enroll…`) | REST | `capsule-api::auth::devices` | [Device Enrollment](/design/device-enrollment/) |
| Version/handshake | REST | `capsule-api` | [Threat Model — Validation](/design/threat-model/validation/) |
| Library queries (timeline, albums, search) | none — client-side over `library.sqlite` | `capsule-core::library` + `db` | [Organization](/design/organization/#system--smart-albums-views) |

Blob bytes use ranged HTTP. The sync feed carries only small opaque envelopes, encrypted metadata,
and blob references; clients fetch content separately with `Range`. Rich content queries have no
server surface because the key-free server cannot evaluate them.

## Why REST/OpenAPI Only

The checked-in OpenAPI **3.1** document is the public contract and the input to Spargen. Kynos owns
routing, middleware composition, and deterministic schema emission. Capsule owns authentication,
protocol headers, error bodies, encrypted-upload state, blob storage ports, and business rules.

One transport keeps negotiation, observability, error handling, streaming, cancellation, and test
harnesses consistent. GraphQL and gRPC are retired architecture, not compatibility surfaces.
Review-only implementations under `legacy-review/` may inform contract tests but cannot be mounted
or restored directly.

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
- Generate the OpenAPI contract twice and assert byte-identical output; generate the Spargen client
  from the checked-in contract and fail CI on drift.
- Exercise streaming upload and ranged download with cancellation, backpressure, retry, and body
  limits without live infrastructure.
- Present valid and invalid federation capabilities through REST and assert compartmentalized,
  indistinguishable rejection behavior.
