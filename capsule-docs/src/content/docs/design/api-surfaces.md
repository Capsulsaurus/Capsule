---
title: API Surfaces
description: Why Capsule's server surface is one REST/OpenAPI contract, and which design doc owns each part of it
status: draft
---

Capsule has one public server transport: [Kynos](https://github.com/getkono/kynos) REST with a
checked-in OpenAPI 3.2 contract. The wire itself (headers, statuses, the exemptions from protocol
negotiation, the error contract) is defined in the server's code and published as
[](/reference/api/conventions/); this page records why the surface has that shape and which design
doc owns each part. Error-code identity is [](/design/i18n/#server-error-codes); publication is
[](/design/developer-docs/).

## Key Design Decisions

- **REST/OpenAPI only.** One transport keeps negotiation, errors, streaming and test harnesses
  uniform, and its semantics do not depend on the HTTP version underneath
  ([](/design/networking/#http-transport)). GraphQL and gRPC are retired and must not return
  ([ADR-0001](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0001-the-public-server-surface-is-kynos-rest-only.md));
  `legacy-review/` may inform contract tests but is never mounted.
- **The document is the contract.** The checked-in OpenAPI document is the public contract and
  Spargen's input. Kynos owns routing, middleware and deterministic schema emission; Capsule owns
  auth, protocol headers, error bodies, upload state, blob ports and business rules.
- **The version is pinned at 3.2.** The server emits with `openapi_as(SpecVersion::V3_2)`, never
  plain `openapi()`, which would pick the lowest version that expresses the API and churn the
  committed contract whenever that changed. `openapi_as` fails on a construct 3.2 cannot express
  instead of dropping the operation.
- **Everything that parses is generated.** Every request and response body, every typed
  parameter, and the byte-serving endpoints (ranged blob fetch, derivative reads) are generated
  from the contract; a hand-written byte path would be a second parser. Only the resumable upload
  state machine is hand-written: chunk scheduling, resumption, retry and the connection budget are
  orchestration over generated calls.
- **One credential scheme.** Session tokens and federation capabilities are distinct token types
  verified by their own modules, but the document declares one `bearer` scheme, so the generated
  client keeps one credential.
- **Status is coarse; the code is the contract.** A client switches on the `error.*` code, never
  the status alone.
- **The server stays key-free.** The sync feed carries opaque envelopes, encrypted metadata and
  blob references; content is fetched separately with `Range`. Content queries (timeline, albums,
  search) have no server surface, because a key-free server cannot evaluate them. The signed
  manifest travels as opaque canonical CBOR, never re-modelled as fields, which would detach its
  signatures. The web client's authenticated view is a key-free projection of the sync feed.

## Surfaces

Each surface's operations are its generated reference page; its design is the owner doc.

| Surface | Operations | Owner doc |
| --- | --- | --- |
| Version and handshake | [](/reference/api/version/) | [](/design/versioning/) |
| Server discovery | [](/reference/api/well-known/) | [](/design/authentication/) |
| Authentication, sessions, devices | [](/reference/api/auth/) | [](/design/authentication/), [](/design/device-enrollment/) |
| Resumable upload | [](/reference/api/upload/) | [](/design/transfer/upload/) |
| Lifecycle writes, roster | [](/reference/api/albums/) | [](/design/authorization/#the-lifecycle-write-surface), [](/design/cryptography/mls/) |
| Sync feed, blob fetch | [](/reference/api/sync/) | [](/design/sync/), [](/design/transfer/download/) |
| Storage verification | [](/reference/api/storage/) | [](/design/transfer/durability/) |
| Share links | [](/reference/api/shares/) | [](/design/share-links/) |
| Guest drops | [](/reference/api/drops/) | [](/design/web-upload/) |
| Quota | [](/reference/api/quota/) | [](/design/quota/) |
| Moderation | [](/reference/api/moderation/) | [](/design/moderation/) |
| Federation; its pull reuses the sync and blob reads with a capability as bearer | [](/reference/api/federation/) | [](/design/federation/) |

## Validation

- Every negotiation rule yields the same headers, status and code across representative routes of
  each module (`capsule-server/tests/conformance.rs`).
- Two generations of the contract are byte-identical and declare OpenAPI 3.2, and the Spargen
  client regenerates without drift (`mise run openapi-check-kynos`).
- The contract lists every operation exempt from the protocol gate
  (`capsule-server/src/openapi/tests.rs`).
- Streaming upload and ranged download handle cancellation, backpressure, retry and body limits
  without live infrastructure.
- Invalid federation capabilities are rejected compartmentalized and indistinguishably.
