---
title: Threat Model
description: How Capsule contains damage from faulty, malicious, or version-mismatched clients
status: draft
---

The server holds no keys and clients write canonical state, so the design must bound what a client can break.

**Invariant:** a faulty, malicious, or version-mismatched client MUST NOT cause **irreparable** damage (lost original bytes, lost audit trail, undetected overwrite of user intent). It may cause only **transient** damage: a quarantined asset, a rejected write with a clear error, a detected-and-reconciled divergence. Key loss is covered by [Cryptography — Failure Modes](/design/cryptography/failure-modes/); this doc covers write-path harm. Primitives and per-structure rules are declared once in their owner docs and only linked here.

| Sub-doc                                            | Concern                                                                 |
| -------------------------------------------------- | ----------------------------------------------------------------------- |
| [Scenarios](/design/threat-model/scenarios/)       | Damage → defense map, quarantine surfaces, provenance immutability      |
| [Validation](/design/threat-model/validation/)     | Server and client refuse-by-default checks, handshake, idempotency, atomicity |
| [Schema Rules](/design/threat-model/schema-rules/) | Schema evolution, forbidden client behaviors, deprecation policy        |

## Client Class Taxonomy

No class is trusted to enforce its own correctness.

| Class          | Holds                                                     | Stopped by                                                                                                                                                                        |
| -------------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Honest**     | Session + access token, device DSK, epoch write-tier key  | Baseline.                                                                                                                                                                          |
| **Faulty**     | Same real keys; buggy writes                              | [Server structural checks](/design/threat-model/validation/#server-side-validation-invariants), client [`verify_asset`](/design/cryptography/keys/#write-authorization), quarantine |
| **Malicious**  | A current DSK and the epoch write-tier key                | Immutable provenance chain, soft-delete, per-album/per-event compartments, signed audit trail                                                                                      |
| **Old**        | Real keys; `X-Capsule-Protocol` below the server window   | [Protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation): `426` before any state is written                                                    |
| **New**        | Real keys; version/suite/schema above the receiver's max  | Refuse-by-default on unknown enums, schemas and versions ([Schema Rules](/design/threat-model/schema-rules/))                                                                      |
| **Web guest**  | Only an [upload-link](/design/web-upload/) capability; no keys, no manifest | Drops land only in the owner's inbox ([quarantined by construction](/design/threat-model/scenarios/#quarantine-surfaces)) under per-link caps, quota and rate limits; enter the library only via trusted [adoption](/design/web-upload/#drop-and-adoption-lifecycle) |

A malicious key-holder cannot be stopped by confidentiality or authentication, so the goal is that it can do nothing **silently**: every write is a signed provenance record, deletes are soft by default, history is append-only.

## Damage Containment Layers

A bug or compromise on one side of a shell cannot cross it.

| Shell                     | Boundary                                                        | Owner                                                                                                     |
| ------------------------- | --------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Per-version               | Album protocol pinning isolates a buggy v_k from v_{k-1} albums | [Versioning](/design/versioning/#album-protocol-version-pinning)                                          |
| Per-album                 | MLS group + per-epoch AMK + write-tier key                      | [MLS](/design/cryptography/mls/), [Keys](/design/cryptography/keys/#album-master-keys-amks)               |
| Per-event                 | Each lifecycle action is its own signed, chained record         | [Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)                        |
| Per-user                  | Owner Group Key; sponsored-account isolation                    | [Keys](/design/cryptography/keys/#owner-group-keys-ogks)                                                  |
| Per-peer (federation)     | Capability tokens, error budgets, new-peer quarantine           | [Federation](/design/federation/)                                                                         |
| Per-device (peering)      | Device directory enforced in the TLS handshake                  | [Peering](/design/peering/#establishing-the-channel)                                                      |
| Per-link (web upload)     | Link caps + inbox isolation                                     | [Web Upload](/design/web-upload/#security-contract)                                                       |
