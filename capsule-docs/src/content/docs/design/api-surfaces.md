---
title: API Surfaces
description: Which server surface speaks which transport, and how negotiation and rejection map across them
status: draft
---

Capsule's server exposes exactly **two** wire transports, chosen per surface: **REST with an OpenAPI schema** for request/response surfaces, and **gRPC** for the sync feed and federation pull. This doc owns the surface ↔ transport map and the cross-transport mapping of the [universal handshake](/design/threat-model/validation/#protocol-and-capability-negotiation) and rejection semantics. The handshake *rules* stay owned by [Threat Model — Validation](/design/threat-model/validation/); error-code identity stays owned by [Internationalization](/design/i18n/#server-error-codes); this doc owns only how both ride each transport.

## Surface ↔ Transport Map

| Surface                                                   | Transport                                    | Crate                                | Owner doc                                                              |
| --------------------------------------------------------- | -------------------------------------------- | ------------------------------------ | ----------------------------------------------------------------------- |
| Authentication (sessions, passkeys, TOTP, OIDC)           | REST                                         | `capsule-api-auth`                   | [Authentication](/design/authentication/)                              |
| Resumable upload (`POST/HEAD/PATCH /upload`)              | REST                                         | `capsule-api-upload`                 | [Import — Upload Protocol](/design/import/upload-protocol/)            |
| Blob fetch (`GET /blob/{hash}`, HTTP `Range`)             | REST                                         | `capsule-api-media`                  | [Import — Download & Sync](/design/import/download-sync/)              |
| Share serving (`/s/{opaque-id}`)                          | REST                                         | `capsule-api-media::shares` (planned) | [Share Links](/design/share-links/)                                    |
| Guest drops (`/u/{opaque-id}/drop`, inbox, adoption)      | REST                                         | `capsule-api-media::drops` (planned) | [Web Upload](/design/web-upload/)                                      |
| Storage verification (`POST /storage/verify`)             | REST                                         | `capsule-api-media::verify` (planned) | [Import — Storage Verification](/design/import/storage-verification/)  |
| Device enrollment (`/auth/devices/enroll…`)               | REST                                         | `capsule-api-auth::devices` (planned) | [Device Enrollment](/design/device-enrollment/)                        |
| Version/handshake surface                                 | REST                                         | `capsule-api`                        | [Threat Model — Validation](/design/threat-model/validation/)          |
| **Sync feed** (change discovery after a cursor)           | **gRPC** — `capsule.sync.v1.SyncService`     | `capsule-api-sync` (stub today)      | [Import — Download & Sync](/design/import/download-sync/)              |
| **Federation pull** (peer server fetches an album's feed) | **gRPC** — same service, capability-gated    | `capsule-api-sync::federation` (planned) | [Federation](/design/federation/)                                      |
| Library queries (timeline, albums, search)                | **none — client-side** over `library.sqlite` | `capsule-core::library` + `db`       | [Organization](/design/organization/#system--smart-albums-views)       |

Two consequences worth stating plainly:

- **Blob bytes always ride REST.** Ranged, resumable byte transfer is what HTTP is best at (`Range`, caches, proxies); gRPC has no ranged-read idiom. The gRPC surface carries only the small, typed feed — content hashes, envelopes, encrypted metadata blobs — never original or derivative bytes.
- **Rich queries have no server surface at all.** The server is key-free and cannot evaluate a content predicate; every timeline, filter, and smart-album query runs client-side against the rebuildable local index. What the server offers is exactly the feed needed to *build* that index.

## Why Two Transports

- **REST + OpenAPI** for every request/response surface: the OpenAPI schema is the machine-readable contract that generates `capsule-sdk` (progenitor), and a plain HTTP surface stays debuggable with nothing but `curl` — which matters for a self-hosted product.
- **gRPC** only where a typed, paged feed contract earns it: the sync feed and its federation twin are one proto contract consumed by every client and every peer server, with the signed manifest traveling as opaque canonical CBOR inside it (never re-modeled as proto fields — re-encoding would detach it from its signatures).

## Legacy: GraphQL (retiring)

`capsule-api-library` exposes an `async-graphql` schema at `/v1/library`. It predates the E2EE key-free server model and is **retiring**, not evolving:

- Its resolvers presume a server that can read content (people, faces, smart tags, memories server-side) — structurally impossible under the [threat model](/design/threat-model/); the key-free replacements are client-side ML ([AI/ML](/design/ai/)) and client-side views ([Organization](/design/organization/#system--smart-albums-views)).
- The generated SDK is OpenAPI-derived and cannot drive GraphQL, so the surface was never consumable by the client stack the design commits to.
- The query role it aimed at is served client-side over `library.sqlite` (above), fed by the sync feed.

The crate stays frozen (compiling, unmounted from new work) until the client-side query path reaches parity; retirement is an explicit slice in the repo-root `SLICES.md`. New surfaces MUST NOT be added to it.

## Negotiation Across Transports

The [universal headers](/design/threat-model/validation/#universal-headers) are defined once, as REST headers. On gRPC they ride call metadata with the lowercased key names; the values and fail-closed rules are identical.

| REST header                  | gRPC call metadata           | Direction         |
| ---------------------------- | ---------------------------- | ----------------- |
| `X-Capsule-Protocol`         | `x-capsule-protocol`         | request           |
| `X-Capsule-Crypto-Suite`     | `x-capsule-crypto-suite`     | request (writes)  |
| `X-Capsule-Sidecar-Schema`   | `x-capsule-sidecar-schema`   | request           |
| `X-Capsule-Protocol-Min`     | `x-capsule-protocol-min`     | response metadata |
| `X-Capsule-Protocol-Max`     | `x-capsule-protocol-max`     | response metadata |
| `X-Capsule-Min-Client-Build` | `x-capsule-min-client-build` | response metadata |

Credentials map the same way: the REST `Authorization: Bearer` access token and the federation capability token both ride the gRPC `authorization` metadata key unchanged — one token format ([EdDSA-JWT](/design/authentication/#access-token)), two carriages.

## Rejection Mapping

REST rejections are HTTP statuses; gRPC rejections are gRPC status codes. The mapping is deliberately coarse — the precise discriminator on **both** transports is the machine-readable [`error.*` code](/design/i18n/#server-error-codes), carried in the REST `ApiError` body and in the gRPC status detail (plus the `x-capsule-error-code` trailing metadata).

| Rejection class                                | REST  | gRPC                  |
| ---------------------------------------------- | ----- | --------------------- |
| Structural (bad envelope, unknown enum, sizes) | `400` | `INVALID_ARGUMENT`    |
| Unauthenticated / expired token                | `401` | `UNAUTHENTICATED`     |
| Unauthorized (capability, quota-hard, suspend) | `403` | `PERMISSION_DENIED`   |
| Not found (and indistinguishable-404 surfaces) | `404` | `NOT_FOUND`           |
| Stale state (chain/cursor/directory regress)   | `409` | `FAILED_PRECONDITION` |
| Payload too large                              | `413` | `RESOURCE_EXHAUSTED`  |
| Protocol version outside `[Min, Max]`          | `426` | `FAILED_PRECONDITION` |
| Rate limited                                   | `429` | `RESOURCE_EXHAUSTED`  |

Where two rejection classes share a gRPC code (`409`/`426`, `413`/`429`), the `error.*` code disambiguates — a client switches on the code, never on the transport status alone.

## Validation

- **Handshake parity (unit).** For each fail-closed rule in [Protocol and Capability Negotiation](/design/threat-model/validation/#protocol-and-capability-negotiation), drive the same bad input through a REST route and the gRPC service; assert the rejection maps per the table above and carries the same `error.*` code.
- **Metadata round-trip (unit).** Send the universal values as gRPC metadata; assert the server's negotiation gate reads them identically to the REST headers.
- **Capability carriage (unit).** Present the same federation capability as a REST bearer and as gRPC `authorization` metadata; assert identical verification outcomes.
