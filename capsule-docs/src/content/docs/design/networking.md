---
title: Network Resilience
description: The connection-class taxonomy, retry policy classes, and the adverse-network posture every transfer surface builds on
status: draft
---

This doc owns three primitives every transfer surface consumes; protocol mechanics stay with the [transfer protocol](/design/transfer/), [sync](/design/sync/), [peering](/design/peering/), and [federation](/design/federation/).

## Connection Classes

A closed enum, evaluated continuously on-device:

| Class | Meaning | Detection |
| --- | --- | --- |
| `unmetered` | Bulk transfer acceptable | OS reports not expensive / not metered |
| `metered` | Bulk deferred | OS reports expensive / metered |
| `constrained` | OS data saver active | iOS Low Data Mode; Android Data Saver |
| `adverse` | Connected but unreliable | Behavioral: repeated resets or stalls within a sliding window; demoted after a clean period |
| `offline` | No usable path | OS path state |

Consumers: [sync criteria](/design/sync/#synchronization-criteria), [staged-upload tiers](/design/transfer/upload/#upload-tiering-staged-uploads), [cache budget](/design/filesystem/client/#automatic-cache-management), [prefetch](/design/transfer/download/#prefetch-and-frugality), and [chunk sizing](/design/transfer/upload/#adaptive-chunk-sizing).

## Path Snapshot

A platform shell (or the CLI's flags) reports one normalized sample of the path: **cost** (unmetered, metered, constrained), **reachability**, **interface** (wired, Wi-Fi, cellular, other), **power** (external, battery, low-power), **thermal** state (nominal, fair, serious, critical) and **execution** (foreground, or background with a deadline). The SDK stamps it with a **path generation**.

- **The generation advances only when reachability, interface or cost changes.** Power, thermal state and execution change the plan, never the path.
- **The transfer plan is a pure function** of the snapshot, the link behaviour the `adverse` detector observed, the server's advice, and the client's compiled window bounds. The server is never in the loop; its advice can only lower a window.
- **Plan rules, in precedence order.** No path, or a critical thermal state, runs nothing. Anything but an unmetered, well-behaved link holds the bulk tiers back, as do low-power mode, a serious thermal state, and a background deadline under one minute; the index tier still runs. Low-power mode and a serious thermal state narrow a lane to one session. A user's force-sync consent lifts a hold on cost or link behaviour, never one on the path, power or heat.
- **A new generation drops what belonged to the old path**: pooled connections are dropped, the `adverse` detector is reset, in-flight work the new plan forbids is cancelled within one scheduling tick, and the work re-plans. A plan that narrows on the same path (a critical thermal state, low-power mode) cancels the same way. Cancelled work resumes from server truth, so a cancel costs at most the bytes in flight.

## Retry Policy Classes

Every retrying surface declares one class. All use exponential backoff with full jitter, honor `Retry-After`, and give up into a user-visible state; none hot-loops.

| Class | Shape | Instances |
| --- | --- | --- |
| `interactive` | short timeout, ≤ 2 retries | auth, on-demand fetches |
| `bulk-transfer` | resume-first (offset / `Range`) | [upload](/design/transfer/upload/#idempotency-and-resumption), [download](/design/transfer/download/#resumption-and-verification) |
| `control/ceremony` | slow ladder, never abandons silently | [MLS recovery](/design/mls-resilience/), federation circuit breaker |

## Adverse-Network Posture

Hostile paths (captive portals, congested carriers, the Great Firewall) MUST degrade throughput, never correctness:

- Every transfer is short and resumable; a lost request costs at most one chunk or window.
- No long-lived stream is required for correctness; any push optimization keeps the paged unary feed as fallback.
- Bulk transfers cut on stall (no bytes for T seconds), not total duration, then resume.
- Under `adverse`, fetches use bounded `Range` windows and uploads the minimum chunk tier.
- Racing happens only at dial (Happy Eyeballs v2).
- Persistent bad WAN: [delegation](/design/peering/#delegation) hands originals to a better-connected device of the same user; the remedy is in-region self-hosting, not circumvention.

## Packet Marking

No correctness or throughput property depends on marking: most access networks strip or rewrite the field, so it carries no meaning end to end. Where a LAN or Wi-Fi network does honour it, marking makes bulk transfers yield to other traffic (home routers often map it to the Wi-Fi WMM background queue), but it gains Capsule no throughput.

- **Bulk transfers are marked Lower Effort** (RFC 8622, DSCP 1): `IP_TOS` on a socket carrying IPv4, `IPV6_TCLASS` on one carrying IPv6, set on a socket the SDK holds before its first byte. The peering channel, which carries hand-offs and sync artifacts, is marked on both ends.
- **Marking is best effort.** A platform that refuses the option, or exposes no setter for the socket's family, leaves the socket unmarked and the transfer proceeds unchanged.
- **Interactive traffic and the server stay unmarked.** Marking may only make Capsule's bulk bytes yield; it never slows a request a person is waiting on. A server-side mark would apply to every accepted connection, interactive ones included, so the server marks nothing.
- **A socket the HTTP stack keeps out of reach stays unmarked.** The SDK's HTTP client dials its own sockets and exposes no hook to them, so HTTP uploads carry the default class until that stack exposes one; the SDK does not reimplement the HTTP connector to reach it.
