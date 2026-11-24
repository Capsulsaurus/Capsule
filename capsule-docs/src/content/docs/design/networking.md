---
title: Network Resilience
description: The one generated HTTP stack, the HTTP transport contract, content coding, connection classes and the path snapshot, per-request admission on metered paths, server advice, retry policy classes, and the adverse-network posture every transfer surface builds on
audit: none
---

This page owns the primitives every transfer surface consumes, and the one HTTP stack every Capsule request leaves through. Protocol mechanics stay with [](/design/transfer/), [](/design/sync/), [](/design/peering/) and [](/design/federation/).

## One HTTP Stack

Every request Capsule sends to a Capsule server is an operation of the committed OpenAPI document ([](/design/api-surfaces/)), built and executed by the client Spargen generates from it. This holds for the SDK every native client and the CLI reach, and for the server's own federation pull. A hand-written request for an operation the document declares is a second parser for a contract that already exists. It drifts from that contract by construction, so it is a defect even when it works.

- **Generated calls only.** Code outside the generated client MUST NOT build a URL, set a header or decode a body for a declared operation. A plain JSON operation the generator already lowers has no exemption.
- **The backend is Spargen's concern.** Spargen's runtime executes requests through a swappable backend: reqwest today, possibly euriops later. Capsule code MUST NOT name the backend's types outside the one adapter that constructs it. Everything Capsule adds to a request is a layer at Spargen's transport seam, typed in the `http` crate's request and response, so it survives a backend swap. That covers protocol negotiation ([](/design/versioning/#protocol-version-source)), token refresh, [](#admission-and-priority), the HTTP/3 race ([](#http-transport)) and stall bounds.
- **An exemption is a generator gap with an issue.** A surface Spargen cannot yet express stays hand-written only while an upstream request for the missing capability is open. The exemption ends in the release that closes the request. The open exemptions are listed in [](#known-gaps).
- **What the generator must provide.** The exemptions close on five capabilities, each requested upstream: `application/cbor` byte bodies; request and response bodies streamed with backpressure and cancellation; `Range` requests with a typed `206` and `416`; per-request context that a caller sets and a transport layer reads (the admission class, the stall bounds); and transport-supplied headers, so a header the transport always sets is not also a per-call argument. Two more are design requests: a resumable-session primitive (create, `HEAD`, a write at an offset, the committed set) for the upload protocol to be expressed over, and a pre-connect socket hook on the backend for [](#packet-marking).
- **Bodies stream through Rust.** A blob's ciphertext is decrypted as it arrives off the generated client's response body, so the decrypting reader is the backpressure and no layer buffers a whole blob ([](/design/transfer/download/#resumption-and-verification)).
- **Outside the rule.** The LAN peering channel is the one Capsule surface that is not an HTTP API. It has its own framing and its own wire spec ([](/design/peering/)). A call to an OIDC provider (its discovery document and keys) is not a Capsule operation. It and the server's federation pull ride the same rustls policy ([](#transport-security)). Map tiles are fetched by the platform's map SDK over the platform's own TLS, outside this stack ([](/design/places/)).

## HTTP Transport

Every Capsule operation is a plain HTTP request and response, and means the same over HTTP/1.1, HTTP/2 and HTTP/3. A faster transport is an optimization the client may take when available, and no protocol property depends on it.

- **No meaning outside request and response.** No operation depends on 1xx interim responses or `Expect: 100-continue`, trailers, server push, stream identifiers, extended `CONNECT`, WebTransport or HTTP datagrams. Integrity travels in headers the client computes before sending, never in trailers. On HTTP/2 and HTTP/3 each request is one stream and cancelling it resets that stream; on HTTP/1.1 cancelling closes the connection. A server that refuses a request before reading its body drains the body or closes gracefully after the response, so the client reads the refusal rather than a reset.
- **Connections are disposable.** Correctness survives the loss of any connection at any moment, because every write is idempotent and resume reads server truth. QUIC connection migration and NAT rebinding, where they work, only spare a reconnect. Nothing relies on them, because a load balancer that does not route by QUIC connection ID breaks them silently.
- **No early data for writes.** The client never sends a non-safe method in TLS 1.3 early data. The server answers `425 Too Early` with `error.request.too_early` to a non-safe method that a terminating proxy forwards with `Early-Data: 1`, before reading its body. The refusal is retryable per [RFC 8470](https://www.rfc-editor.org/rfc/rfc8470): the client re-sends the same request once the handshake has completed, outside early data. Early data MAY carry `GET` and `HEAD`, which are idempotent even where `HEAD` starts a pending finalization.
- **HTTP/3 is opportunistic and terminates at the proxy.** The client learns of it from `Alt-Svc` or an HTTPS DNS record, races it against TCP at dial, and keeps an expiring per-network record that UDP is unusable. A QUIC path that stops making progress falls back to TCP at the next request boundary, and nothing is resumed mid-request across transports.
  - The race is between connection attempts. A QUIC dial that loses to TCP is not a failure: it may finish and serve a later request. One that fails while TCP connected records UDP unusable on that network, and so does a race in which both dials fail, so later requests on that network do not wait for the QUIC dial again.
  - QUIC never bypasses a proxy. A client with a proxy configured for `https` destinations sends every `https` request through it over TCP and does not race QUIC. A proxy configured only for `http` destinations does not stop the race: it carries no `https` request, and only `https` origins race.
  - A QUIC path has stopped making progress when it receives nothing for half the stall bound while a request rides it; keep-alives every quarter of the stall bound make a live path answer within that. The request fails, UDP is recorded unusable, and the retry's re-send rides TCP.
  - Only a request that is safe to send twice rides QUIC: an idempotent method, or an upload chunk, which is addressed by offset and checked by checksum. Any other request rides TCP whatever is advertised. A request that fails on QUIC is re-sent once over TCP; one whose TCP leg already failed reaches the retry engine as that failure.
  - Every response's `Alt-Svc`, on either transport, feeds discovery. Beyond the path-progress check above, riding QUIC adds no bound: a request is cut under the same stall and response-wait bounds it runs under over TCP.
  - An advertisement is believed only for an `https` origin, and the alternative endpoint must present a certificate valid for the origin.
- **Parallelism is connections, not streams.** On a lossy or high-latency link a single TCP connection's throughput is bounded by its round trip and loss rate, and HTTP/2 multiplexes every stream onto that one window. Bulk transfers therefore SHOULD spread across several connections, within the in-flight windows ([](/design/transfer/upload/#queueing-across-servers)), where the platform's HTTP stack allows it. Correctness never depends on it.
- **Windows cover the bandwidth-delay product.** Flow-control and socket buffers (on QUIC, the stream and connection windows and the UDP socket buffers) are sized to the target bandwidth times round trip, within a fixed memory budget. The sender owns congestion control (the client on upload, the server on download), and neither side steers the other's controller.
- **Bulk and interactive traffic use separate pools.** Each origin has an interactive connection pool and a bulk one. A queue of chunk uploads never holds the connection a login or a visible thumbnail is waiting for. Only bulk sockets are ever marked ([](#packet-marking)).
- **Every decoded body is bounded.** The client caps each response body, after content decoding, at a limit set per operation, before it parses a byte. A body past the cap is a malformed response and is never retried as is. A client does not trust a server's sizes ([](/design/threat-model/)). An unbounded body, or a small encoded body that inflates, would otherwise exhaust a phone's memory.

## Compression

The server decides what is compressed, per operation. A proxy never decides. [ADR-0019](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0019-the-server-compresses-an-allowlist-of-json-and-cbor-responses.md) records the trade.

- **An explicit allowlist.** The server mounts Kynos's response compression only on the operations it lists by operation id. Each one returns JSON or CBOR, is a list or a feed, and is compressed only above a size threshold. The sync feed, the federation pull listings, the receipt and activity logs, the device list and the public discovery records qualify. An operation absent from the list is sent with identity coding, so a new operation is uncompressed until someone decides otherwise.
- **Never ciphertext.** `application/octet-stream` is never compressed. Sealed bytes do not compress, and a ranged response's byte offsets name the identity octets.
- **Never a secret beside attacker input (BREACH).** An operation whose response carries a token, a code, a capability, a TOTP secret or wrapped key material is never on the list, whatever its size. That excludes the `auth`, `totp`, `oidc`, `enrollment` and `escrow` tags, capability minting and refresh, and share and drop provisioning. Compressing such a body would let a party that influences the rest of it read the secret off the length.
- **Codings.** SDK clients send `Accept-Encoding: zstd, gzip`, and the server prefers zstd. A browser sets `Accept-Encoding` itself, so the web client takes what the browser negotiates and still applies the decoded-body cap. No client compresses a request body. Upload bodies are ciphertext, and no JSON request is large enough to matter.
- **Caches see the variation.** Kynos's compression layer sends `Vary: Accept-Encoding` on every response it covers, encoded or not.
- **Proxies are pass-through.** A reverse proxy in front of Capsule MUST NOT add, strip or change a content coding. A proxy that compresses on its own would compress the excluded operations too. [](/guides/self-hosting/) owns the proxy configuration.
- **Bounded on decode.** The decoded-body cap above applies to the decoded length, so a compressed response cannot become a decompression bomb.

## Transport Security

- TLS 1.3 is required wherever both ends support it, and clients try it first. TLS 1.2 is the absolute floor, for legacy ingress only; nothing lower is accepted.
- Deployments SHOULD enable hybrid X25519+ML-KEM key exchange on 1.3.
- Ingress, not the application server, terminates TLS. This is operator-configured and documented, not enforced.
- Capsule's own TLS (the SDK, LAN [](/design/peering/) mTLS, server egress) is rustls, never native-tls or OpenSSL, under the same policy.

## Connection Classes

A closed enum, derived from the [](#path-snapshot) and evaluated on every change to it:

| Class         | Meaning                  | Detection                                                                                   |
| ------------- | ------------------------ | ------------------------------------------------------------------------------------------- |
| `unmetered`   | Bulk transfer acceptable | OS reports not expensive / not metered                                                      |
| `metered`     | Bulk deferred            | OS reports expensive / metered                                                              |
| `constrained` | OS data saver active     | iOS Low Data Mode; Android Data Saver                                                       |
| `adverse`     | Connected but unreliable | Behavioral: repeated resets or stalls within a sliding window; demoted after a clean period |
| `offline`     | No usable path           | OS path state                                                                               |

Consumers: [](/design/sync/#synchronization-criteria), [](/design/transfer/upload/#upload-tiering-staged-uploads), [](/design/filesystem/client/#automatic-cache-management) (cache budget), [](/design/transfer/download/#prefetch-and-frugality), and [](/design/transfer/upload/#adaptive-chunk-sizing).

## Path Snapshot

A platform shell (or the CLI's flags) reports one normalized sample of the path: **cost** (unmetered, metered, constrained), **reachability**, **interface** (wired, Wi-Fi, cellular, other), **power** (external, battery, low-power), **thermal** state (nominal, fair, serious, critical) and **execution** (foreground, or background with a deadline). The SDK stamps it with a **path generation**.

- **Native shells MUST report the path.** Every native client feeds `FfiSession::set_path` from the OS listeners: `NWPathMonitor` (`isExpensive`, `isConstrained`), thermal-state and low-power notifications on Apple; `ConnectivityManager` network callbacks (`NET_CAPABILITY_NOT_METERED`, Data Saver), `PowerManager` and thermal status on Android. It reports once when a session opens, before any transfer, and again whenever a listener fires. The SDK never probes the network itself.
- **An unreported path is metered.** Until the shell's first report, a session plans as reachable and `metered`, so a shell that forgets to report loses bulk transfer instead of spending a user's data. Only a desktop shell, or the CLI, may report `unmetered` without an OS signal.
- **One source of the class.** The connection class every consumer reads (cache budget, staged tiers, admission) derives from this snapshot alone.
- **The generation advances only when reachability, interface or cost changes.** Power, thermal state and execution change the plan, never the path.
- **The transfer plan is a pure function** of the snapshot, the link behaviour the `adverse` detector observed, the server's advice ([](#server-advice)), and the client's compiled window bounds. The server is never in the loop; its advice can only lower a window.
- **Plan rules, in precedence order.**
  1. No path, or a critical thermal state, runs nothing.
  2. Anything but an unmetered, well-behaved link holds the bulk tiers back, as do low-power mode, a serious thermal state, and a background deadline under one minute. The index tier still runs.
  3. Low-power mode and a serious thermal state narrow a lane to one session.
  4. A user's force-sync consent lifts a hold on cost or link behaviour, never one on the path, power or heat.
- **A new generation drops what belonged to the old path**: pooled connections are dropped, the `adverse` detector is reset, and the work re-plans. Bulk work the new plan forbids pauses at its next chunk boundary: no new upload chunk or `Range` window starts. A request already in flight is cancelled within one scheduling tick if the new path is metered, constrained or gone, and is otherwise allowed to finish. A plan that narrows on the same path (a critical thermal state, low-power mode) pauses the same way. Paused and cancelled work resumes from server truth, so a pause costs at most the bytes in flight.

## Admission and Priority

Whether a request may leave, and in what order, is decided per request, at Spargen's transport seam, before a byte is sent. One layer there reads the request's **class**, the current [](#path-snapshot) and the user's policy. It answers one of four ways: **send**; **hold** until the path changes; **ask** the user; or **refuse**.

- **Every request has a class.** An interactive request is one a person is waiting on: auth, a sync feed page, a control operation, a single metadata record. Bulk requests carry the transfer scheduler's priority class (visible, near-view, user-requested original, prefetch, sync, backup), which the scheduler owns with its ordering and viewport rules ([](/design/transfer/download/#transfer-scheduler)). The SDK maps every generated operation to a default class, and a test fails on an operation that has none. An operation that serves several classes, like a blob read, defaults to its most restrictive one, and the caller raises it.
- **Cancellation is per request.** The scheduler cancels a request when its reason goes (a cell scrolls away, focus moves). The seam drops its body stream at once, and resume reads server truth.
- **The policy is per class: allow, ask or deny.** It is set per connection class. On `unmetered` every class is allowed. On `adverse` the bulk classes are held by the plan rules above, and on `offline` every class is held, whatever the policy says. The defaults for the two cost classes:

| Class                                                                                            | `metered` | `constrained`              |
| ------------------------------------------------------------------------------------------------ | --------- | -------------------------- |
| interactive, and index-tier records ([](/design/transfer/upload/#upload-tiering-staged-uploads)) | allow     | allow                      |
| visible, near-view (thumbnail and preview rungs)                                                 | allow     | allow, thumbnail rung only |
| user-requested original                                                                          | ask       | ask                        |
| prefetch, sync (bulk reconciliation), backup                                                     | deny      | deny                       |

- **Ask raises `ApprovalRequired`.** The event names the class, the item count, the estimated bytes and the connection class. It goes out once per user action or batch, never once per request. Native code renders it as a platform prompt ([](/design/clients/#metered-prompt)). The answer is _once_, _always on this kind of network_ (which changes the user's policy) or _not now_ (which holds the work). A consent covers that batch on the current path generation only, and a new generation voids it. In the background, _ask_ is _hold_: no prompt is raised from the background.
- **Deny is not silent.** Held and refused bulk work is counted in the diagnostics snapshot ([](/development/performance/#typed-stage-events-and-the-diagnostics-snapshot)), and a client shows it as waiting for Wi-Fi.
- **Force sync is consent.** The user's force-sync ([](/design/sync/#notifications)) answers _once_ for every bulk class on the current generation.
- **The layer belongs to the SDK.** Admission sits at the transport seam rather than in each caller, so a hand-written path cannot skip it, and a backend swap keeps it.

## Server Advice

The server tunes clients it cannot redeploy through **advice**: one `X-Capsule-Advice` response header on session create and `HEAD`, an [RFC 9651](https://www.rfc-editor.org/rfc/rfc9651) structured-field dictionary of integers. Its members are `chunk` (suggested chunk bytes), `sessions` (in-flight sessions per lane), `ranges` (in-flight ranges per session) and `ttl` (seconds the advice holds). A session's own expiry travels beside it, not in it ([](/design/transfer/upload/#session-lifetime-and-discard)).

- **Clamped.** The client clamps every field into bounds compiled into it, uses its own default for a field that is absent or unparseable, and ignores fields it does not know. A window field can only lower the client's own cap: the effective window is the smaller of the two. Advice older than its time to live is dropped, except a session's `ranges`, which is fixed for the session's life.
- **Advisory only.** No field can weaken integrity checks, exceed the client's memory, disk or retry budgets, override the user's cost preferences or the cost class, or outlast the OS's execution grant. Those invariants are enforced in the core and are never server-configurable.
- **Additive.** A new field is a new optional member; an older client ignores it and is no worse off.

## Retry Policy Classes

Every retrying surface declares one class. All use exponential backoff with full jitter, honor `Retry-After`, give up into a user-visible state, and never hot-loop. Errors are classified before any retry: transport failures, `5xx`, `408`, `425` and `429` are retryable, and contract `4xx` codes are terminal unless the recovery matrix in [](/design/transfer/upload/#idempotency-and-resumption) names an action. Besides each item's own attempt cap, each server lane holds one shared retry budget, so a server that fails every request costs a bounded number of retries rather than one ladder per queued item.

| Class              | Shape                                  | Instances                                                                                                           |
| ------------------ | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `interactive`      | short timeout, ≤ 2 retries             | auth, on-demand fetches                                                                                             |
| `bulk-transfer`    | resume-first (committed set / `Range`) | [](/design/transfer/upload/#idempotency-and-resumption), [](/design/transfer/download/#resumption-and-verification) |
| `control/ceremony` | slow ladder, never abandons silently   | [](/design/mls-resilience/), federation circuit breaker                                                             |

## Adverse-Network Posture

Hostile paths (captive portals, congested carriers, the Great Firewall) MUST degrade throughput, never correctness:

- Every transfer is short and resumable, so a lost request costs at most one chunk or window.
- No long-lived stream is required for correctness; any push optimization keeps the paged unary feed as fallback.
- Bulk transfers cut on stall (no bytes for T seconds), not total duration, then resume. The server applies it to a request body it is reading, the client to every body it sends or receives. Once a request body is fully sent, the client waits for the response under a separate, longer bound that covers the server's write and fsync. Waiting on a session whose finalization is still running is not a stall; it is observed by polling `HEAD`.
- Under `adverse`, fetches use bounded `Range` windows and uploads a shorter target request duration.
- Racing happens only at dial (Happy Eyeballs v2, and HTTP/3 against TCP); a request is never sent twice in parallel.
- On a persistently bad WAN, delegation ([](/design/peering/#delegation)) hands originals to a better-connected device of the same user. The remedy is in-region self-hosting, not circumvention.

## Packet Marking

No correctness or throughput property depends on marking: most access networks strip or rewrite the field, so it carries no meaning end to end. Where a LAN or Wi-Fi network honours it, marking makes bulk transfers yield to other traffic (home routers often map it to the Wi-Fi WMM background queue) but gains Capsule no throughput.

- **Bulk transfers are marked Lower Effort** (RFC 8622, DSCP 1): `IP_TOS` on a socket carrying IPv4, `IPV6_TCLASS` on one carrying IPv6, set on a socket the SDK holds before its first byte. The peering channel, which carries hand-offs and sync artifacts, is marked on both ends.
- **Marking is best effort.** A platform that refuses the option, or exposes no setter for the socket's family, leaves the socket unmarked, and the transfer proceeds unchanged.
- **Interactive traffic and the server stay unmarked.** Marking may only make Capsule's bulk bytes yield; it never slows a request a person is waiting on. The server marks nothing, for two reasons. A server-side mark would apply to every accepted connection, interactive ones included. And past the LAN, transit and access networks bleach the field, so a mark set at a data centre would reach no network that honours it. Marking is a client-side, LAN-side tool only.
- **A socket the HTTP stack keeps out of reach stays unmarked.** The backend under Spargen's runtime dials its own sockets and exposes no hook to them. HTTP uploads therefore carry the default class until the backend offers a pre-connect socket hook ([](#one-http-stack)). The mark then goes on the bulk pool's sockets only ([](#http-transport)). The SDK does not reimplement the HTTP connector to reach the socket, because that connector is what supplies Happy Eyeballs at dial.

## Known Gaps

- **No native shell reports the path.** Neither app calls `FfiSession::set_path`, and a session starts from the desktop default, so phones plan every transfer as `unmetered` on cellular. The unreported-is-metered default is not implemented either.
- **Two class sources.** The cache budget samples its own connection-class port (`capsule-sdk::cache_budget`) beside the session's path monitor (`capsule-sdk::net::path`).
- **No admission layer.** Classes, the allow/ask/deny policy, `ApprovalRequired` and the two connection pools do not exist. The reconciliation gate refuses even metadata-only sync off `unmetered`, against the defaults table.
- **Hand-written wire.**
  - The four `application/cbor` operations are omitted from generation (`capsule-sdk/build.rs`), waiting on the generator's CBOR byte bodies.
  - The chunked upload protocol and its session listing are hand-written (`capsule-sdk::upload`), as is the ranged blob read (`capsule-sdk::fetch`), which buffers each window whole.
  - Album provisioning (`capsule-sdk::albums`) and storage verification (`capsule-sdk::verify`) are hand-written JSON calls with no exemption.
  - The SDK names reqwest well beyond the backend adapter: negotiation, refresh, the HTTP/3 router and every hand-written path.
  - The web client calls the API with hand-written `fetch` (`capsule-web/src/lib/api.ts`) rather than the generated client built for wasm.
- **Nothing is compressed.** Neither the server's Kynos features nor the SDK's reqwest features enable a content coding. No response body is capped after decoding.

## Validation

- **Generated-only (unit).** A test fails on a request built outside the generated client for an operation the document declares, other than a listed exemption.
- **Admission (unit).** Every generated operation maps to a class. Each row of the defaults table yields its outcome on each connection class. A new path generation voids a consent. _Ask_ in the background holds and raises nothing.
- **Unreported path (unit).** A session that has had no report plans as `metered`.
- **Path feeding (smoke, per platform).** A simulated switch from Wi-Fi to cellular reaches `set_path`, and the next chunk is held.
- **Compression allowlist (unit).** Every allowlisted operation is JSON or CBOR and carries no excluded tag. Every other operation's response has no `Content-Encoding`.
- **Decode cap (unit).** A gzip or zstd bomb is refused at the cap without allocating past it.
