---
title: REST API
description: The auth model, negotiation headers, and error contract common to every Capsule endpoint
status: draft
---

Capsule's server surface is REST over HTTP, described by a single OpenAPI 3.2 document that
`capsule-server` emits from its own route types. This page is the hand-written half of that
reference: the things true of every endpoint, which a per-endpoint page would otherwise repeat
fifty-nine times. The endpoints themselves are the generated pages listed in the sidebar.

Read [API Surfaces](/design/api-surfaces/#surface--transport-map) first if you want the map from
a surface to the module that owns it and the design document that explains it. This reference
says what the wire looks like; that one says why.

## The server holds no keys

The most important thing to know before reading any endpoint: **every write is sealed on the
client before it is sent, and the sync feed returns opaque envelopes.** The server stores,
addresses, and serves ciphertext, and authorizes who may do so. It cannot read an asset, and no
endpoint accepts a plaintext one.

That is also why this reference has no try-it panel. A playground could exercise the version
handshake, the auth flows, and blob fetch; everything else would either return ciphertext or
reject an unsealed body, and a reader who succeeded at it would leave believing the API accepts
plaintext. The honest equivalent is the [command line](/reference/cli/), which performs the
sealing.

## Authentication

Credentials ride the standard `Authorization: Bearer` header. An access token is short-lived and
issued by `POST /v1/auth/login`; `POST /v1/auth/refresh` rotates the pair. Each generated page
marks every operation as requiring authentication or not, read from the document's own security
requirements rather than from prose here.

An account with a confirmed second factor does not get a session from `login` — it gets a
challenge, and the sign-in finishes at `POST /v1/auth/login/verify-totp`. A client that treats
`202` as a failure will appear to work until the first user enables TOTP.

A `401` carries a `WWW-Authenticate` challenge, per RFC 9110.

## Negotiation

Every public route applies the same headers, which the generated pages do not repeat per
operation:

| Header | Direction |
| --- | --- |
| `X-Capsule-Protocol` | request |
| `X-Capsule-Crypto-Suite` | request for writes |
| `X-Capsule-Sidecar-Schema` | request |
| `X-Capsule-Protocol-Min` | response |
| `X-Capsule-Protocol-Max` | response |
| `X-Capsule-Min-Client-Build` | response |

`GET /v1/version` is the unauthenticated reachability probe a client performs before the
handshake. It has no failure variant by construction. What a server publishes about itself —
attestation keys, capabilities, announced deprecations, revoked token identifiers — is under
[Server discovery](/reference/api/well-known/).

## Errors

Failures are `application/problem+json` (RFC 9457) bodies. Beyond the standard members, every
problem this server renders carries a **`code`**: a stable identifier from the `error.*` catalog
namespace described in [Internationalization](/design/i18n/). Clients localize the code; the
`detail` message stays English.

**Switch on the code, never on the status alone.** HTTP status is deliberately coarse:

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

A `404` is sometimes a deliberate indistinguishability, not an absence: a surface that must not
reveal whether a resource exists to an unauthorized caller answers the same way in both cases.

## Clients

Do not hand-write a client. `capsule-sdk` is generated from this same document, and the
generated request and response types are the only ones guaranteed to track it. The layering
rules server code follows are [API Practices](/development/api-practices/).

## How these pages stay true

`capsule-server` emits `capsule-server/openapi.json` from its route types — no database, no key
material, no network, because the router is built purely to describe it — and
`mise run openapi-check-kynos` fails the Rust gate if the committed document disagrees with the
server. The documentation build reads that file and nothing else.

A generated page is never edited. If something on it is wrong, the annotation it came from is
wrong: fix the handler or model documentation, run `mise run openapi-kynos`, and commit the
document. The pipeline and the reasoning behind it are
[Developer Documentation](/design/developer-docs/).
