Capsule's server surface: one REST contract every client, and every federated server, speaks. The
server holds no keys. Every write is sealed on the client before it is sent, and the sync feed
returns opaque envelopes, so no operation accepts or returns a plaintext asset.

## Protocol negotiation

| Header | Direction | Rule |
| --- | --- | --- |
| `X-Capsule-Protocol` | request | Required on every gated operation. |
| `X-Capsule-Crypto-Suite` | request | Sent on writes; validated when present. |
| `X-Capsule-Sidecar-Schema` | request | Validated when present; absence is not refused. |
| `X-Capsule-Protocol-Min`, `-Max` | response | On every response, including `401`, `413`, `426` and an unrouted `404` or `405`. |
| `X-Capsule-Min-Client-Build` | response | On every response. Advisory; `0.0.0` means no cutoff. |
| `X-Request-Id` | response | On every response: a UUIDv7 the server mints. An inbound value is ignored, never echoed. |

- A **write** (any method but `GET` and `HEAD`) whose protocol is outside `[Min, Max]` is
  `426 error.protocol.version_unsupported`; the window is only in the headers.
- A **read** (`GET` or `HEAD`) is admitted at any well-formed protocol date, past or future.
- A missing or malformed header on a gated operation is `400 error.request.malformed`.
- The advertised window and the enforced one come from one source and cannot differ.

These operations skip the request gate and still carry the response headers:

- `GET /v1/version`: the probe a client makes before it knows the window.
- `GET /.well-known/capsule/*`: public discovery.
- `/s/{opaque_id}` and its `wrapped-secret` and `blob/{hash}` reads: a `426` would break a share
  link's indistinguishable `404`.
- `POST /d/{opaque_id}` and `PATCH`/`GET /d/{opaque_id}/{upload_id}`: the drop link pinned its
  versions when it was issued.
- `POST /v1/auth/refresh` and `POST /v1/auth/logout`: refusing them would cut off reads that are
  still allowed, or keep open a session its holder closed. Login and register stay gated.

## Credentials

One `bearer` security scheme, `Authorization: Bearer`, carries both kinds of credential: a
session's access token and a federation capability. Each is verified by the module that issued
it, and the generated client keeps one credential.

## Errors

Every rejection is an RFC 9457 problem whose `code` member is a stable `error.*` catalog code.
A client switches on `code`, never on the status alone, and localizes it itself. `request_id`
repeats the response's `X-Request-Id`: a client shows it for the user to quote and never parses
it. It is declared on every problem but never required, so a client stays lenient toward a
server that predates it.

The common rejection classes and their statuses follow; each operation lists every status it
can return.

| Rejection | Status |
| --- | --- |
| Structural: bad envelope, unknown enum, sizes | `400` |
| Unauthenticated or expired token | `401` |
| Unauthorized: capability, hard quota, suspension | `403` |
| Not found, including the indistinguishable-`404` surfaces | `404` |
| Stale state: chain, cursor, directory regression | `409` |
| Gone: a superseded or deleted record | `410` |
| Payload too large | `413` |
| Unsupported chunk media type | `415` |
| A write forwarded as TLS early data | `425` |
| Protocol outside `[Min, Max]` | `426` |
| Rate limited | `429` |
| A fault the server can explain, such as a store outage | `503` with `Retry-After` |
| Storage full | `507` |
| Unexplained internal state, logged at `ERROR` | `500` |
