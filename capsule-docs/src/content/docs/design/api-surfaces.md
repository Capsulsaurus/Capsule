---
title: API Surfaces
description: The Kynos REST/OpenAPI server surface and its negotiation and rejection contracts
status: draft
---

Capsule has one public server transport: **Kynos REST with a checked-in OpenAPI 3.2 contract**. This doc owns the surface-to-module map, the document model, and the HTTP carriage of the [universal handshake](/design/threat-model/validation/#protocol-and-capability-negotiation) (whose rules Threat Model — Validation owns). Error-code identity is owned by [Internationalization](/design/i18n/#server-error-codes); publication by [Developer Documentation](/design/developer-docs/).

## Surface ↔ Transport Map

| Surface | Module | Owner doc |
| --- | --- | --- |
| Authentication (sessions, TOTP, OIDC) | `capsule-server::auth` | [Authentication](/design/authentication/) |
| Resumable upload (`POST /v1/upload`, `HEAD/PATCH /v1/upload/{id}`) | `capsule-server::upload` | [Transfer — Upload](/design/transfer/upload/) |
| Lifecycle writes (`POST /v1/albums/{album_id}/ops`) | `capsule-server::routes::ops` | [Authorization](/design/authorization/#the-lifecycle-write-surface) |
| Album roster (`PUT /v1/albums/{album_id}/roster`) | `capsule-server::membership` | [Threat Model — Validation](/design/threat-model/validation/) |
| Blob fetch (`GET /v1/blob/{hash}`, `Range`) | `capsule-server::blob` | [Transfer — Download](/design/transfer/download/) |
| Sync feed | `capsule-server::sync` | [Transfer — Download](/design/transfer/download/) |
| Federation capabilities and report intake | `capsule-server::federation` | [Federation](/design/federation/) |
| Federation pull — no own route: `GET /v1/sync?album_id=` and `GET /v1/blob/{hash}` with a capability as bearer | `capsule-server::federation` | [Federation](/design/federation/) |
| Share serving (`/s/{opaque_id}`) | `capsule-server::share` | [Share Links](/design/share-links/) |
| Guest drops (`POST /d/{opaque_id}`, inbox, adoption) | `capsule-server::drop` | [Web Upload](/design/web-upload/) |
| Storage verification (`POST /v1/storage/verify`) | `capsule-server::verify` | [Transfer — Durability](/design/transfer/durability/) |
| Device enrollment (`/v1/auth/devices/enroll`) | `capsule-server::enrollment` | [Device Enrollment](/design/device-enrollment/) |
| Version / handshake | `capsule-server` | [Threat Model — Validation](/design/threat-model/validation/) |
| Library queries (timeline, albums, search) | none — client-side over `library.sqlite` | [Organization](/design/organization/#system--smart-albums-views) |

The sync feed carries only opaque envelopes, encrypted metadata and blob references; content is fetched separately with `Range`. Content queries have no server surface because the key-free server cannot evaluate them. The signed manifest travels as opaque canonical CBOR, never re-modelled as wire fields (re-encoding would detach its signatures). The web client's authenticated view is a key-free projection of the sync feed; in-browser decrypt is post-v1.

## Why REST/OpenAPI Only

The checked-in OpenAPI document is the public contract and Spargen's input. Kynos owns routing, middleware and deterministic schema emission; Capsule owns auth, protocol headers, error bodies, upload state, blob ports and business rules. One transport keeps negotiation, errors, streaming and test harnesses uniform. GraphQL and gRPC are retired and MUST NOT return; `legacy-review/` may inform contract tests but is never mounted.

### The Document Is 3.2, and Its Version Is Pinned

The server emits with `openapi_as(SpecVersion::V3_2)`, not plain `openapi()`, which would pick the lowest version that expresses the API and churn the committed contract when that changes. Enabling Kynos's `openapi32` feature alone does not produce 3.2 (see the Kynos row in [Dependencies](/design/dependencies/#rust)). `openapi_as` errors on a construct 3.2 cannot express rather than silently dropping operations.

### What Is Generated, and What Is Not

**Generated** from the contract: every request and response body, every typed parameter, and the byte-serving endpoints (blob fetch with `Range`, derivative reads). A hand-written byte path would be a second parser.

**Hand-written**: only the resumable upload state machine — chunk scheduling, offset resumption, retry laddering, connection budget — which is orchestration over generated calls, not parsing.

## Negotiation Across Transports

Every public route applies the same headers:

| Header | Direction |
| --- | --- |
| `X-Capsule-Protocol` | request; required on every gated route |
| `X-Capsule-Crypto-Suite` | request on writes; validated when present |
| `X-Capsule-Sidecar-Schema` | request; validated when present, absence not refused |
| `X-Capsule-Protocol-Min` / `-Max` | response, on every response (including `401`, `413`, `426`, and an unrouted `404`/`405`) |
| `X-Capsule-Min-Client-Build` | response, on every response; advisory (`0.0.0` = no cutoff) |
| `X-Request-Id` | response, on every response (including an unrouted `404`/`405`); a UUIDv7 the server mints per request. An inbound `X-Request-Id` is ignored, never echoed |

Rules (carriage in `capsule-server/src/negotiation.rs`; pinned by `capsule-server/tests/conformance.rs`):

- A **write** whose `X-Capsule-Protocol` is outside `[Min, Max]` is `426` with `error.protocol.version_unsupported`; the window is only in the headers.
- A **read** is admitted at any grammatical protocol date, past or future.
- A missing or malformed header on a gated operation is `400 error.request.malformed`.
- Advertised and enforced windows come from one source and cannot differ.

**Exempt from the request gate** (still carrying response headers):

- `GET /v1/version` — the probe before a client knows the window.
- `GET /.well-known/capsule/attestation-keys`, `GET /.well-known/capsule/server-info`, `GET /.well-known/capsule/deprecation`, `GET /.well-known/capsule/revoked-jti` — public discovery.
- `GET /s/{opaque_id}`, `/s/{opaque_id}/wrapped-secret`, `/s/{opaque_id}/blob/{hash}` — a `426` would break [Share Links](/design/share-links/)' indistinguishable `404`.
- `POST /d/{opaque_id}`, `PATCH /d/{opaque_id}/{upload_id}` — the link pins versions at issuance ([Web Upload](/design/web-upload/)).
- `POST /v1/auth/refresh`, `POST /v1/auth/logout` — refusing them would cut off reads still allowed, or keep open a session its holder closed. Login and register stay gated.

Credentials use `Authorization: Bearer`. Session tokens and federation capabilities are distinct token types verified by their modules, but the document has exactly **one `bearer` security component** for both, so the generated client uses one credential key.

## Rejection Mapping

Status is coarse; clients MUST switch on the `error.*` code in the `ApiError` body, never on status alone.

Every problem body also carries a `request_id` member: the same id as that response's `X-Request-Id` header, which names the request's server log lines. The document declares it on every problem schema but never as required, so a client stays lenient toward a server that predates it; a client surfaces it for a user to quote, and never parses or switches on it.

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
| Server fault the server can explain (store outage) | `503` with `Retry-After` |
| Storage full | `507` |
| Unexplained internal state | `500`, logged at `ERROR` |

## Validation

- Every handshake rule yields the same headers, status and code across representative routes of each module.
- Two generations of the contract are byte-identical and declare OpenAPI 3.2; the Spargen client regenerates without drift.
- Streaming upload and ranged download handle cancellation, backpressure, retry and body limits without live infrastructure.
- Invalid federation capabilities are rejected compartmentalised and indistinguishably.
