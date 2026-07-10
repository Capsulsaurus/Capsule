---
title: Network Resilience
description: The connection-class taxonomy, retry policy classes, and the adverse-network posture every transfer surface builds on
status: draft
---

Capsule clients run on phones with metered plans, hotel Wi-Fi, and networks that silently drop long-lived connections. This doc owns the three cross-cutting primitives every transfer surface consumes: the **connection-class taxonomy** (the input to sync criteria and cache budgets), the **retry policy classes** (so per-doc retry ladders are instances of a shared shape, not reinventions), and the **adverse-network posture** (the design assumptions that keep Capsule usable on hostile networks). Protocol mechanics stay owned by their protocol docs — [upload](/design/import/upload-protocol/), [download & sync](/design/import/download-sync/), [peering](/design/peering/), [federation](/design/federation/); this doc owns only the shared taxonomy and posture.

## Connection Classes

A closed enum, evaluated continuously on-device:

| Class | Meaning | Detection inputs |
| --- | --- | --- |
| `unmetered` | Bulk transfer is acceptable | iOS `NWPathMonitor` (`isExpensive == false`); Android `NET_CAPABILITY_NOT_METERED`; desktop default |
| `metered` | Byte-counted link; bulk deferred | `isExpensive`; metered capability absent |
| `constrained` | OS-level data saver active | iOS `isConstrained` (Low Data Mode); Android Data Saver |
| `adverse` | Connectivity is present but unreliable | **Behavioral promotion**: ≥ N mid-transfer resets, stalls, or black-holes within a sliding window (thresholds are client-tunable, not protocol surface); demoted after a clean period |
| `offline` | No usable path | OS path state |

Consumers: the [sync criteria](/design/import/download-sync/#synchronization-criteria) (small vs. large reconciliation), the [staged-upload tier gates](/design/import/download-sync/#upload-tiering-staged-uploads), the [cache-eviction byte budget](/design/filesystem/client/#automatic-cache-management), [prefetch](/design/import/download-sync/#prefetch-and-frugality), and [adaptive chunk sizing](/design/import/upload-protocol/#adaptive-chunk-sizing) (under `adverse`, the conservative choice is the tier minimum). `adverse` is deliberately behavioral — no OS reports it, and the networks that need it most (see below) look "connected" to every OS API.

## Retry Policy Classes

Three named classes; every retrying surface declares which one it instantiates. Universal rules: exponential backoff **with full jitter**, always; honor server backoff signals (`Retry-After`, 429/503); every retry loop has a bounded give-up that surfaces a user-visible state; no surface ever hot-loops.

| Class | Shape | Instances (owned where they are) |
| --- | --- | --- |
| `interactive` | short timeout, ≤ 2 retries, then a visible failure state | auth flows, on-demand tier fetches |
| `bulk-transfer` | resume-first (HEAD offset / `Range`), patient within the session's lifetime, backoff between attempts | [upload resume](/design/import/upload-protocol/#idempotency-and-resumption), [download resume](/design/import/download-sync/#resumption-and-verification), [transient fetch retry](/design/import/download-sync/#tiered-on-demand-fetch) |
| `control/ceremony` | slow ladder, long horizon, never abandons silently | [MLS recovery ladder](/design/mls-resilience/) (30 s → 2 min → 10 min), [federation circuit breaker](/design/federation/) (5/30/60 min) |

## Adverse-Network Posture

Some networks — congested carrier links, captive-portal hotels, and notably paths crossing China's Great Firewall — exhibit connectivity that no error code explains: connections reset mid-transfer, long-lived streams die silently after minutes, throughput collapses unpredictably, and the same request succeeds on retry. A server outside mainland China (Hong Kong included) serving a user inside it should expect all of the above as the steady state, not an incident. Capsule's answer is **resilience by shape, not workarounds**: the protocols are built so that hostile paths degrade throughput, never correctness.

- **Assume mid-transfer resets and silent black-holing.** Every transfer is short, independent, and resumable; losing any single request loses at most one chunk/window of progress. This already falls out of the upload protocol (chunked, offset-resumable) and ranged blob fetch — stated here as a *requirement* on any future surface.
- **No long-lived stream is ever required for correctness.** The sync feed is a unary, paged `Sync(cursor, page_size)` call — short-poll by construction. Any future push/streaming optimization MUST keep the unary equivalent as the fallback path, because long-lived connections are precisely what unreliable middleboxes kill.
- **Stall detection over total timeouts.** Bulk transfers cut on *no-bytes-for-T-seconds*, not on a total-duration timeout (which either kills slow-but-live transfers or waits forever on dead ones), then resume from the offset. A stall counts toward `adverse` promotion.
- **Bounded transfer windows under `adverse`.** Blob fetches shrink to bounded `Range` windows and uploads sit at the chunk-size tier minimum, so each request is small enough to usually complete between resets.
- **Connection racing at dial only.** Happy Eyeballs v2 (parallel IPv6/IPv4 dial) is cheap and standard. Racing whole *requests* across paths is not done in v1 — it doubles server load for marginal gain.
- **LAN peering is the degraded-mode alternative.** When the WAN path is persistently adverse, [peering](/design/peering/) moves originals between same-user devices without touching it; the staged-upload T0 index still escapes over the thin link.
- **The structural remedy is locality.** Resilience keeps Capsule *usable* across a hostile path; it cannot make one fast. For a persistently degraded region, the answer is self-hosting in-region — Capsule is built for that — not circumvention machinery in the client, which is out of scope.

## Validation

- **Class detection (unit).** Mocked OS signals per platform map to the expected class; `constrained` and `metered` are distinguished.
- **Adverse promotion/demotion (unit).** An injected reset/stall pattern promotes to `adverse` and switches fetches to bounded windows; a clean period demotes.
- **Stall-cut-resume (smoke).** Black-hole a `Range` fetch mid-window; the stall detector cuts within its bound and the resume transfers zero duplicate bytes.
- **Backoff discipline (unit).** Every policy class's retry sequence stays within its bounds, jitters, and honors `Retry-After`; no configuration can produce an unbounded hot loop.
