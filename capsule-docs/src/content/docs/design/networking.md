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

Capsule sets no DSCP value or IP traffic class on its own sockets. No correctness or throughput property depends on marking: most access networks strip or rewrite the field, so it carries no meaning end to end. Where a LAN or Wi-Fi network does honour it, it can only lower the priority of bulk transfers relative to other traffic, not speed them up.
