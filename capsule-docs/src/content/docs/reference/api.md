---
title: REST API
description: How Capsule's generated REST reference is organized, and what to read before an endpoint
status: draft
---

Capsule's server surface is REST over HTTP, described by one OpenAPI 3.2 document `capsule-server`
emits from its own route types. Every page in this section is generated from that document; why
the surface is shaped this way is [](/design/api-surfaces/).

- **Read [](/reference/api/conventions/) first.** It is the contract every endpoint shares:
  protocol negotiation and the operations exempt from it, the bearer credential, and how a
  rejection is coded and mapped to a status.
- **The server holds no keys.** Every write is sealed on the client, and the sync feed returns
  opaque envelopes, so no endpoint takes a plaintext asset. That is why there is no try-it
  panel: a request a reader could send by hand would teach that the API accepts plaintext. The
  [command line](/reference/cli/) performs the sealing instead.
- **Each endpoint page** opens with its operations and schemas, then sets each operation beside
  an example request and example responses. The document itself is
  [`/openapi.json`](/openapi.json).
- **Do not hand-write a client.** `capsule-sdk` is generated from the same document; its request
  and response types are the only ones guaranteed to track it. Server code follows
  [](/development/api-practices/).

## How these pages stay true

`mise run openapi-check-kynos` fails the Rust gate when the committed
`capsule-server/openapi.json` disagrees with the server, and the docs build reads that file and
nothing else. A wrong page is fixed at the handler or model annotation it came from, then
regenerated with `mise run openapi-kynos`. The pipeline is [](/design/developer-docs/).
