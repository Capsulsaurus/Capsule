---
title: Core Principles
description: The core principles that guide the design and development of Capsule
---

These principles apply universally to all components of Capsule from clients to server.

Determinism and idempotent processes. Raw and original data is the source of truth
All data is processed aligned to 4KiB (matches memory and disks). Just verify no edge cases require a smaller or bigger multiple though.
Forward and backwards compatibility: old clients ignore new fields and new clients tolerate missing ones gracefully

Data integrity: We can NEVER delete data unexpectedly. We act under strict scenarios and crash early otherwise. We implement multiple layers of safeguards to avoid current and future bugs. We trust data in the server will be safe (and in robust hardware) and data in the clients as potentially lost.
Treat most data as ephemeral. If it wasn’t original data, it can be rebuilt.
Encryption, security, and isolation: Keep sensitive code that require auditing and storage of data separate. Encrypt metadata besides data. Compartmentalize every boundary as a failure-containment boundary — per-album, per-peer, per-event, per-user, per-version — so a bug or compromise on one side of a boundary cannot cross it.
Divide between offline and online functionalities: a feature should work either solely online or offline. It should not exhibit different behaviours depending on resource connectivity. This simplifies business logic and risk of state shifts.

**Recovery-First**: The filesystem must be reconstructible from partial corruption. No database is required to interpret critical data — sidecar files are the canonical metadata store; the database is a rebuildable query cache.

**Self-Describing**: Each media file is paired with a CBOR sidecar containing all user-editable and stable metadata. Files are independently interpretable without a running database.

**Atomic Writes**: Use temp-file + rename throughout. Direct overwrites risk corruption on power loss.

**Postel's Law**: Liberal in what we accept *within a known schema version* — unknown sidecar fields are preserved verbatim and missing optional fields are tolerated. **Cross-version is closed**: a structure announcing a schema version (`sidecar_schema`, `crypto_suite_id`, `protocol_version`) above the receiver's max known is rejected, never best-effort-parsed. The asymmetry is what prevents a faulty or new client from silently corrupting state — see [Threat Model — Schema Evolution and Field Grammar](/design/threat-model/#schema-evolution-and-field-grammar).

## Single Source of Truth

Every primitive, construction, format, or component identity Capsule depends on is **declared in exactly one design doc**. Other docs reference the declaration by anchor; they never restate the choice. The goal is that swapping a primitive (a hash, a model, a container format) is a single-doc edit, not a 10-doc cascade that silently leaves inconsistencies.

The owner docs are:

| Domain                                                      | Owner doc                                                     |
| ----------------------------------------------------------- | ------------------------------------------------------------- |
| All cryptographic primitives + constructions                | [Cryptography](/design/cryptography/#primitives-inventory)    |
| ML model identities                                         | [ML Models and Algorithms](/design/ml-models/)                |
| LQIP scheme + thumbnail/preview formats                     | [Thumbnails and Previews](/design/thumbnails/)                |
| Server storage stack + topology                             | [Filesystem](/design/filesystem/)                             |
| Session/access tokens + auth flow                           | [Authentication](/design/authentication/)                     |
| Backup artifact container + escrow                          | [Backup and Recovery](/design/backup-recovery/)               |
| CRDT scheme, identifiers, geolocation                       | [Metadata](/design/metadata/)                                 |
| Upload/download protocol semantics                          | [Import and Synchronization](/design/import-synchronization/) |
| Federation trust model, capability tokens, soft-fail policy | [Federation](/design/federation/)                             |
| LAN discovery + peer channel                                | [Peering](/design/peering/)                                   |
| Album protocol version pinning                              | [Versioning](/design/versioning/)                             |
| Stacking taxonomy + trash semantics                         | [Asset Organization](/design/organization/)                   |
| Lifecycle action set                                        | [Authorization](/design/authorization/)                       |
| Damage containment, client-class taxonomy, server-side validation duties | [Threat Model](/design/threat-model/)            |

**Permitted secondary mentions.** Mechanism-explanatory phrasing inside a non-owner doc is fine — for example, "STREAM tags catch chunk reordering" inside [Peering](/design/peering/) is explaining a *behavior*, not declaring a *choice*. What the rule forbids is restating the choice itself ("we use SHA-256") outside the owner doc. When in doubt, link.

## Damage Containment

A faulty, malicious, or version-mismatched client must not be able to inflict irreparable damage on user data. The principles above (data integrity, atomic writes, recovery-first, self-describing, Postel's Law, encryption + compartmentalization) name the *posture*; the [Threat Model](/design/threat-model/) names the *defenses*.

In particular, the threat model owns:

- The **client class taxonomy** (honest, faulty, malicious, old, new) — how each is authenticated and what stops each from doing harm.
- The **damage scenario → invariant map** — for every concrete attack or bug class, the single owner doc that defeats it.
- **Server-side validation invariants** — the refuse-by-default structural checks a key-less server runs on every write.
- **Protocol and capability negotiation** — the universal fail-closed handshake that rejects version mismatches before any state is written.
- **Idempotency, atomicity, and quarantine** rules that span owner docs.

Each owner doc grows a short section pointing into the relevant threat-model section, but the cross-cutting statements live there. Principles continues to own the universal *posture*; threat model owns the universal *defenses*.
