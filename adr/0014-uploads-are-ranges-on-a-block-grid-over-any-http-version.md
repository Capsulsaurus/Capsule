# ADR-0014 — Uploads are ranges on a block grid, over any HTTP version, steered only by clamped advice

- **Status:** accepted
- **Date:** 2026-10-29
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Transfer — Upload](../capsule-docs/src/content/docs/design/transfer/upload.md#chunk-rules-and-strictness), [Network Resilience](../capsule-docs/src/content/docs/design/networking.md#http-transport)
- **Issues:** #1040, #1041, #1042, #1043, #1044, #1045, #1046, #1047, #1050

## Context

The transfer protocol was reviewed before ratification. The review set out from design criteria drawn up from first principles: getting HTTP/3's operational advantages for long uploads and downloads that must be reorderable, run from a Rust core shared by native clients, react to OS network hints, and survive high-latency, lossy and unstable links. The overriding criterion was the fewest ways to misbehave at runtime.

Upload was a strictly sequential append: a chunk had to land at exactly the received-byte count, with one chunk in flight per session. Effectively one session ran per server. On a 600 ms round trip one connection carries at most a chunk per round trip, and a lossy long path holds a single TCP flow to tens of KB/s, whatever the chunk size. Four further defects made the protocol misbehave at runtime:

- **Finalization blocked a request.** The request that completed a session waited for finalization's re-hash, which a reverse proxy can time out.
- **The chunk deadline was a total time.** A slow but steady chunk was cut, and the client halved its chunk size towards 4 KiB.
- **A lost ACK could fail the blob.** A lost ACK followed by a resized re-send was a terminal `chunk_conflict`.
- **No transport contract.** Nothing was written down for HTTP versions, early data, or how far the server may steer a client.

A proposal for an HTTP/3-native design was reviewed with the protocol. It used a long-lived control stream, server-switched congestion control, server-sent `PRIORITY_UPDATE` and `STOP_SENDING` for reordering, 0-RTT writes, datagram telemetry, and OS hints relayed to the server for a decision.

## Decision

An upload session is a 4 KiB grid, frozen at creation. A `PATCH` writes one range. Admission is checked atomically against the committed set and the in-flight ranges before any byte is written:

| Range | Outcome |
| --- | --- |
| Disjoint from both | Committed |
| Wholly inside the committed set | A duplicate, answered with the session's state |
| Any other overlap | `range_conflict` |

Reordering is the client's schedule only. Sequential upload is the schedule with one range in flight. Throughput comes from several sessions and ranges in flight over several connections, within client-local windows that the server may only lower. No request waits on finalization; a failed finalization's code is read from `HEAD`. The server cuts only a stalled chunk body.

Every operation means the same over HTTP/1.1, HTTP/2 and HTTP/3. HTTP/3 is opportunistic and terminates at the proxy. Correctness never depends on connection migration, and 0-RTT never carries a write.

The server steers clients only through `X-Capsule-Advice`, which the client clamps to compiled bounds. The client reacts to OS hints locally, through a normalized path snapshot, and the server is never in that loop.

## Consequences

- **No write can touch a committed byte.** The re-hash at finalize stays the backstop for a staging bug. A lost ACK, a resize, a crash between fsync and record, or a duplicate request all converge, and none of them fail the blob.
- **More server state, but no per-write records.** The session store records an interval set rather than a counter, bounded at 64 fragments, and admission tracks in-flight claims. In-flight claims are process-local; a restart drops them, and their ranges are simply re-sent. A claim is released only after its writes return. A re-send wholly inside the committed set is a duplicate whatever its bytes, so `chunk_conflict` and `offset_mismatch` are retired. A changed source is caught by the client's source guards and, failing those, by the finalize re-hash, which costs an upload but never stores wrong bytes. The protocol date is bumped.
- **Guest drops follow the same detached finalization.** Drops therefore gain a status probe under the link capability, and the web client, version-locked to its server, reads its outcome there.
- **Re-sends are guarded against nonce reuse.** A range re-derived from a changed plaintext under the same key and nonce would give the server two AES-GCM ciphertexts under one nonce. Re-sends are routine with ranges in flight in parallel, so a re-derived chunk whose STREAM tag differs from the one recorded at bundle build is never sent.
- **The protocol no longer forbids background transfer.** Requests are self-contained and order-free, so an OS transfer daemon could run several queued chunks of one blob. The in-flight cap and the recovery rows still bind whatever schedules them, so background transfer needs its own design.
- **Mechanisms rejected because they fail on the networks this protocol targets or are unavailable in the stacks it uses.** Each one below is rejected because it cannot work as intended, adds a failure mode, or has no HTTP/1.1 or HTTP/2 fallback:
  - a long-lived control stream, WebTransport, extended `CONNECT` and HTTP datagrams;
  - server-driven `PRIORITY_UPDATE` (HTTP/3 sends it only from the client, and it never governs a request body);
  - `STOP_SENDING` as a reprioritization tool;
  - server-chosen congestion control (the sender owns it, which is the client on upload);
  - flow-control steering (credit only grows, so withholding it stalls the sender rather than shaping its rate);
  - 0-RTT writes (replay);
  - content-defined chunking (no gain on compressed media);
  - tail-chunk hedging (wasted metered bytes on a session whose writes are idempotent anyway);
  - relaying OS hints to the server for a decision (the round trip fails exactly when the network does).
- **The code lags the contract until the linked issues land:** non-blocking finalize, the stall deadline, block-grid admission, advice and expiry headers, the SDK range scheduler and source guard, the in-flight windows, the path snapshot, restart-safe downloads, and the drop status probe.
