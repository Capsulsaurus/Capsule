---
title: Network Resilience
description: The HTTP transport contract, connection classes and the path snapshot, server advice, retry policy classes, and the adverse-network posture every transfer surface builds on
status: draft
---

This doc owns the primitives every transfer surface consumes: the HTTP transport contract, connection classes and the path snapshot they are derived from, server advice, retry policy classes and the adverse-network posture. Protocol mechanics stay with the [transfer protocol](/design/transfer/), [sync](/design/sync/), [peering](/design/peering/), and [federation](/design/federation/).

## HTTP Transport

Every Capsule operation is a plain HTTP request and response, and means the same over HTTP/1.1, HTTP/2 and HTTP/3. A faster transport is an optimization the client may take when it is available; no property of the protocol depends on it.

- **No meaning outside request and response.** No operation depends on 1xx interim responses or `Expect: 100-continue`, trailers, server push, stream identifiers, extended `CONNECT`, WebTransport or HTTP datagrams. Integrity travels in headers the client computes before sending, never in trailers. On HTTP/2 and HTTP/3 each request is one stream, and cancelling it resets that stream; on HTTP/1.1 cancelling closes the connection. A server that refuses a request before reading its body drains the body or closes gracefully after the response, so the client reads the refusal rather than a reset.
- **Connections are disposable.** Correctness survives the loss of any connection at any moment, because every write is idempotent and resume reads server truth. QUIC connection migration and NAT rebinding, where they work, only spare a reconnect; a load balancer that does not route by QUIC connection ID breaks them silently, which is why nothing relies on them.
- **No early data for writes.** The client never sends a non-safe method in TLS 1.3 early data. The server answers `425 Too Early` with `error.request.too_early` to a non-safe method that a terminating proxy forwards with `Early-Data: 1`, before reading its body. The refusal is retryable, as [RFC 8470](https://www.rfc-editor.org/rfc/rfc8470) makes it: the client re-sends the same request once the handshake has completed, outside early data. Early data MAY carry `GET` and `HEAD`, which are idempotent even where `HEAD` starts a pending finalization.
- **HTTP/3 is opportunistic and terminates at the proxy.** The client learns of it from `Alt-Svc` or an HTTPS DNS record, races it against TCP at dial, and keeps a per-network record that UDP is unusable, with an expiry. A QUIC path that stops making progress falls back to TCP at the next request boundary; nothing is resumed mid-request across transports.
- **Parallelism is connections, not streams.** On a lossy or high-latency link a single TCP connection's throughput is bounded by its round trip and loss rate, and HTTP/2 multiplexes every stream onto that one window. Bulk transfers therefore SHOULD spread across several connections, within the [in-flight windows](/design/transfer/upload/#queueing-across-servers), where the platform's HTTP stack allows it; correctness never depends on it.
- **Windows cover the bandwidth-delay product.** Flow-control and socket buffers are sized to the target bandwidth times round trip, within a fixed memory budget. The sender owns congestion control: the client on upload and the server on download. Neither side steers the other's controller.

## Connection Classes

A closed enum, derived from the [path snapshot](#path-snapshot) and evaluated on every change to it:

| Class | Meaning | Detection |
| --- | --- | --- |
| `unmetered` | Bulk transfer acceptable | OS reports not expensive / not metered |
| `metered` | Bulk deferred | OS reports expensive / metered |
| `constrained` | OS data saver active | iOS Low Data Mode; Android Data Saver |
| `adverse` | Connected but unreliable | Behavioral: repeated resets or stalls within a sliding window; demoted after a clean period |
| `offline` | No usable path | OS path state |

Consumers: [sync criteria](/design/sync/#synchronization-criteria), [staged-upload tiers](/design/transfer/upload/#upload-tiering-staged-uploads), [cache budget](/design/filesystem/client/#automatic-cache-management), [prefetch](/design/transfer/download/#prefetch-and-frugality), and [chunk sizing](/design/transfer/upload/#adaptive-chunk-sizing).

## Path Snapshot

Each platform shell observes its operating system and pushes one normalized **path snapshot** into the Rust core; no transfer logic runs in the shell. The snapshot carries:

| Field | Values | Sources |
| --- | --- | --- |
| cost | unmetered, metered, constrained | `NWPath` `isExpensive` / `isConstrained`; Android `NET_CAPABILITY_NOT_METERED` and Data Saver; desktop OS metered flag; CLI flag |
| reachability | satisfied, unsatisfied | OS path status |
| interface | Wi-Fi, cellular, wired, other | OS path |
| path generation | a counter | incremented by the core when reachability, interface or cost changes |
| power | normal, saver | Low Power Mode; Android battery saver |
| thermal | nominal, elevated, critical | `ProcessInfo.thermalState`; Android thermal status |
| execution | foreground, or background with a deadline | the OS's execution grant |

The core derives the connection class and the transfer plan as a pure function of **(snapshot, observed link behaviour, [server advice](#server-advice), compiled bounds)**, which is table-tested without a device. Observed link behaviour is the resets and stalls the behavioural `adverse` detection counts. Reaction is local and immediate: the server is never in the decision loop, because the network that degrades is the one a round trip to it would need. The path generation advances only when reachability, interface or cost changes, never on a minor path update. A new path generation drops pooled connections, resets the behavioural `adverse` detection, cancels in-flight work the new cost class forbids, and re-plans. Power saver and elevated thermal state lower the in-flight windows; a background deadline stops new requests that cannot finish within it.

## Server Advice

The server tunes clients it cannot redeploy through **advice**: one `X-Capsule-Advice` response header on session create and `HEAD`, an [RFC 9651](https://www.rfc-editor.org/rfc/rfc9651) structured-field dictionary of integers. Its members are `chunk` (suggested chunk bytes), `sessions` (in-flight sessions per lane), `ranges` (in-flight ranges per session) and `ttl` (seconds the advice holds). A session's own expiry travels beside it, not in it ([Session Lifetime](/design/transfer/upload/#session-lifetime-and-discard)).

- **Clamped.** The client clamps every field into bounds compiled into it, uses its own default for a field that is absent or unparseable, and ignores fields it does not know. A window field can only lower the client's own cap: the effective window is the smaller of the two. Advice older than its time to live is dropped, except a session's `ranges`, which is fixed for the session's life.
- **Advisory only.** No field can weaken integrity checks, exceed the client's memory, disk or retry budgets, override the user's cost preferences or the cost class, or outlast the OS's execution grant. Those invariants are enforced in the core and are never server-configurable.
- **Additive.** A new field is a new optional member; an older client ignores it and is no worse off.

## Retry Policy Classes

Every retrying surface declares one class. All use exponential backoff with full jitter, honor `Retry-After`, and give up into a user-visible state; none hot-loops. Errors are classified before any retry: transport failures, `5xx`, `408`, `425` and `429` are retryable; contract `4xx` codes are terminal unless the recovery matrix names an action. Besides each item's own attempt cap, each server lane holds one shared retry budget, so a server that fails every request costs a bounded number of retries rather than one ladder per queued item.

| Class | Shape | Instances |
| --- | --- | --- |
| `interactive` | short timeout, ≤ 2 retries | auth, on-demand fetches |
| `bulk-transfer` | resume-first (committed set / `Range`) | [upload](/design/transfer/upload/#idempotency-and-resumption), [download](/design/transfer/download/#resumption-and-verification) |
| `control/ceremony` | slow ladder, never abandons silently | [MLS recovery](/design/mls-resilience/), federation circuit breaker |

## Adverse-Network Posture

Hostile paths (captive portals, congested carriers, the Great Firewall) MUST degrade throughput, never correctness:

- Every transfer is short and resumable; a lost request costs at most one chunk or window.
- No long-lived stream is required for correctness; any push optimization keeps the paged unary feed as fallback.
- Bulk transfers cut on stall (no bytes for T seconds), not total duration, then resume. Both sides apply it: the server to a request body it is reading, the client to every body it sends or receives. Once a request body is fully sent, the client waits for the response under a separate, longer bound that covers the server's write and fsync. Waiting on a session whose finalization is still running is not a stall; it is observed by polling `HEAD`.
- Under `adverse`, fetches use bounded `Range` windows and uploads a shorter target request duration.
- Racing happens only at dial (Happy Eyeballs v2, and HTTP/3 against TCP); a request is never sent twice in parallel.
- Persistent bad WAN: [delegation](/design/peering/#delegation) hands originals to a better-connected device of the same user; the remedy is in-region self-hosting, not circumvention.

## Packet Marking

Capsule sets no DSCP value or IP traffic class on its own sockets. No correctness or throughput property depends on marking: most access networks strip or rewrite the field, so it carries no meaning end to end. Where a LAN or Wi-Fi network does honour it, marking can make bulk transfers yield to other traffic, but it gains Capsule no throughput.
