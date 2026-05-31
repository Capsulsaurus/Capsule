---
title: Schema Rules and Open Questions
description: Schema evolution, forbidden client behaviors, deprecation policy, and unresolved design questions
---

Capsule schemas evolve over time, but the rules of evolution are fixed — what fields a writer may add, what a receiver may safely ignore, what fields are closed enums, and what timing/grammar rules apply. Each schema's owner doc defines its fields; this doc defines what evolution is allowed across them. Schema-rule enforcement lives in `capsule-core::crypto` (sidecar/manifest decode) and the validation layers of every API crate.

## Schema Evolution and Field Grammar

### Deny-by-Default for Unknown Request Fields

[Postel's Law](/design/principles/#postels-law-asymmetric) — as tightened in principles — applies asymmetrically:

- **In requests (client → server, or peer → server):** unknown fields at known positions in a known schema are accepted and preserved verbatim (an unknown CBOR key *inside* a known manifest). Unknown fields at the **top level** that the receiver does not declare are **rejected** (a stray key at the request root). Schema-bearing requests that announce a `sidecar_schema` or `crypto_suite_id` the receiver does not implement are rejected outright. The asymmetry is deliberate: liberal acceptance in requests is what lets new clients write extensions, but only *inside* a known schema envelope.
- **In responses (server → client):** unknown fields are preserved verbatim. A new server sending an old client a response with a new field does not break the old client.

### Closed Enums

**Every enum in a signed or validated structure is closed per `protocol_version`** — a value outside the set known at that version is a structural error, never a "future value to ignore." This is a blanket rule, not a curated list, so it cannot rot: adding a value to *any* such enum bumps `protocol_version` (see [Versioning — Album Protocol Version Pinning](/design/versioning/#album-protocol-version-pinning)), and a pinned old album never sees the new value. It is enforced on **both sides** — the server's structural envelope check (invariant 16) and the client's `verify_asset`/decode path (see [Validation](/design/threat-model/validation/)).

The authoritative value set for each enum lives in its owner doc — `AssetManifest.action` in [Authorization](/design/authorization/#the-closed-action-set), `content_type` and `gps.source` in [Metadata](/design/metadata/#sidecar-schema-v1), `DerivativeManifest.role` in [Provenance](/design/cryptography/provenance/#derivative-provenance) — never duplicated here.

### Timestamp Grammar

All `timestamp` and `ts` fields are RFC 3339 strings, **self-asserted and audit-only** — never load-bearing for authorization or ordering, which ride the provenance chain and the MLS epoch ([Keys — Write Authorization](/design/cryptography/keys/#write-authorization)). The server records its own trusted `received_at` for any time-based policy.

A server-side **sanity bound** (default ±30 days of server wall-clock, deployment-configurable) is applied to writes only: a gross-drift guard that surfaces an honest client with a faulty NTP rather than silently distorting its audit trail. It is explicitly *not* a security control. Reads serve whatever timestamp was historically recorded.

### Bounded String and Collection Sizes

Every field has a maximum length declared in the schema (e.g. `caption_lww.value ≤ 4096 bytes`; `superseded_captions ≤ 16 entries`). The receiver rejects an oversized value. No field is unbounded.

## Forbidden Client Behaviors

This is **not a standalone contract** — each entry is the negative of a rule owned by another doc, consolidated here only as an index. The defense never depends on clients honoring the list: the receiving server and the client-side `verify_asset` chokepoint reject the *consequence* structurally regardless (that is the entire point of [refuse-by-default validation](/design/threat-model/validation/#server-side-validation-invariants)). A client that does any of these is **buggy by definition**, and the prohibition is enforced where its rule lives:

| A correct client never…                                          | Enforced / owned by                                                                                                      |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Re-signs a manifest under a lower `crypto_suite_id` (downgrade)  | [Primitives — Versioning](/design/cryptography/primitives/#versioning-identifiers)                                       |
| Signs for an album epoch it does not hold the write-tier key for | [Keys — Write Authorization](/design/cryptography/keys/#write-authorization)                                             |
| Issues an OR-set remove for an `add_id` it never observed        | [Metadata — Add-id Binding](/design/metadata/#add-id-binding)                                                            |
| Strips `_unknown` or `superseded_captions` on write-back         | [Metadata](/design/metadata/#collaborative-metadata) — the signature covers them                                         |
| Overwrites or truncates a provenance chain                       | [Provenance — Append-Only](/design/cryptography/provenance/#chained-append-only-structure)                               |
| Submits `revoke_all_sessions` without master-key proof           | [Authentication](/design/authentication/)                                                                                |
| Decodes non-home-peer bytes outside the sandbox                  | [Clients — Sandboxed Decoder](/design/clients/#sandboxed-decoder)                                                        |
| Silently promotes an AI tag to a user tag (must be a signed op)  | [Metadata — Tag Provenance](/design/metadata/#tag-provenance-and-namespacing)                                            |
| Retries a `429` / `409` / `426` with the same payload            | back off / re-align / upgrade first — [Validation](/design/threat-model/validation/#protocol-and-capability-negotiation) |

## Min-Supported-Client Deprecation Policy

Dropping a `protocol_version` from the server's accepted window is a breaking change. The policy:

1. **Announcement.** A deprecation cutoff date is published at `/.well-known/capsule/deprecation` ahead of the cutoff by at least the announcement window (default 90 days, deployment-configurable). The announcement names the cutoff date and the minimum `protocol_version` that will remain accepted.
2. **Server response.** Below the cutoff, every response carries `X-Capsule-Min-Client-Build` and a `Warning:` header pointing to the deprecation URL.
3. **Hard cutoff.** On the cutoff date, the dropped version moves outside `[Min, Max]`. Writes from clients pinned to that version receive `426`. Reads still succeed.
4. **Stranded user.** A user whose only client is below the cutoff still has every recovery path from [Cryptography — Failure Modes](/design/cryptography/failure-modes/): master key, cross-device, OGK, backup artifact. The deprecation does not strand data; it strands a specific old binary.

The deprecation surface is **never** retroactive against historical state. Old albums pinned to a dropped version remain readable forever — they just cannot be written to from a current client.

## Open Questions

One design question remains open — and it is **deliberately deferred to v2**, not a v1 blocker:

1. **Cross-server album replication (v2).** v1 pins each album to a single home server; v2 will need a story for cross-server MLS state and federated commit ordering.

The following questions have since been **resolved** and now live in their owner docs, not here:

- *Restore-vs-stale-revival* → [Backup & Recovery — Backup Verification](/design/backup-recovery/#backup-verification) (restore is a chain-reconciliation; newer local state always wins).
- *Sync cursor authenticity* → [Download & Sync](/design/import/download-sync/#discovering-what-changed) (server-MAC'd cursor + client monotonic check) and [Validation invariant 22](/design/threat-model/validation/#on-the-sync-feed-directory-publish-and-federated-reports).
- *Sponsored-account write damage* → [Cryptography — Keys: Damage bound under sponsor compromise](/design/cryptography/keys/#damage-bound-under-sponsor-compromise).
- *AMK epoch monotonicity bootstrap* → [Cryptography — Write Authorization](/design/cryptography/keys/#write-authorization) (ceiling anchored to the MLS commit chain).
- *Cross-language deterministic CBOR* → [Metadata — Canonical CBOR Encoding](/design/metadata/#canonical-cbor-encoding) (normative ruleset + blocking conformance gate).
- *Federated quota DoS via honest user* → [Quota — Accounting Model](/design/quota/#accounting-model) (receiving-user attribution + per-peer cache cap).
- *"New client" read surface* → [Clients — Reading State From a Newer Client](/design/clients/#reading-state-from-a-newer-client).
