# ADR-0026 — Every Capsule request goes through the generated client, and policy lives at its transport seam

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Network Resilience — One HTTP Stack](../capsule-docs/src/content/docs/design/networking.md#one-http-stack), [Network Resilience — Admission and Priority](../capsule-docs/src/content/docs/design/networking.md#admission-and-priority)
- **Issues:** #440, #1099

## Context

[ADR-0002](0002-clients-are-generated-from-the-committed-contract.md) drew the line between
generated and hand-written code at parsing versus orchestration. The code drew it somewhere else.
`capsule-sdk/build.rs` generated the typed client from `capsule-server/openapi.json`, but
hand-written wire remained in four places:

- **CBOR.** The four `application/cbor` operations were omitted by `OmitRule`
  (`capsule-sdk/build.rs`, #440), because the generator did not classify the media type.
- **Upload.** The chunked upload protocol and its session listing built raw requests
  (`capsule-sdk/src/upload.rs`), described as "too stateful for codegen".
- **Ranged reads.** The ranged blob read built its own `Range` header and buffered each window
  whole (`capsule-sdk/src/fetch.rs`).
- **Plain JSON.** Album provisioning (`capsule-sdk/src/albums.rs`) and storage verification
  (`capsule-sdk/src/verify.rs`) had no reason to be hand-written at all.

Around those paths the SDK named reqwest throughout: protocol negotiation, token refresh, the
HTTP/3 router and every hand-written call. Protocol screening was implemented twice, once as
a layer over the generated client and once for hand-written sends.

Three changes made the split costly. Metering had to be enforced per request, and a policy any
hand-written path can bypass enforces nothing. The backend might change from reqwest to
another (euriops). And the generator's own transport seam had grown into a typed, backend-neutral
interceptor chain, so there was somewhere to put policy that every request crosses.

## Decision

Every request to a Capsule server is a generated operation, executed through Spargen's runtime.
Capsule adds behaviour only as layers at Spargen's transport seam, typed in the `http` crate:
negotiation, refresh, admission, the HTTP/3 race and stall bounds. Capsule never names the
backend outside the adapter that builds it. The plain JSON calls move to generated calls
now. Upload and ranged reads become generator feature requests: streamed bodies, `Range` with a
typed `206`, per-request context, transport-supplied headers, a resumable-session primitive and a
pre-connect socket hook. The only Capsule surface outside the document is the LAN peering
channel.

## Consequences

- **"Too stateful for codegen" is retired as a reason.** The upload protocol's state machine
  stays hand-written orchestration. Its requests become generated calls once the generator
  streams bodies, and the hand-written wire is an exemption with an end date, not a category.
- **One screening layer.** Negotiation, admission and refresh run once, at the seam, for every
  request. The parallel `send_negotiated` path goes with the hand-written wire.
- **Packet marking waits on the backend, not on Capsule.** #1099's blocker moves from "reqwest
  exposes no socket" to a backend requirement Capsule states once. The SDK still never
  reimplements a connector.
- **Background transfer keeps the rule.** On iOS the OS daemon, not the app, ships a background
  upload. The core builds each request with the generated client and writes it out as a spec
  with a sealed body file, so no native code builds a request.
- **The web client is the last exception.** It calls the API with hand-written `fetch` until the
  generated client is built for wasm. It is version-locked and read-only, which bounds the
  drift, but the exception is tracked like the others.
- **Rejected: a Capsule-owned HTTP client beside the generated one.** It would put back the
  second parser ADR-0002 removed, and it would leave a backend swap touching every caller.
