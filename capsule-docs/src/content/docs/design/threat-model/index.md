---
title: Threat Model
description: How Capsule contains damage from faulty, malicious, or version-mismatched clients
---

E2EE shifts most of the trust to the client. The server holds no keys; clients write the canonical state. That makes the question "what damage can a client cause?" load-bearing for the design — a single buggy implementation, a hostile keyholder inside an album, a stranded old build, or a too-new prototype all have to fail safely.

A faulty, malicious, or version-mismatched client must not be able to cause **irreparable** damage (loss of original bytes, loss of audit trail, undetected silent overwrite of user intent) and should not be able to cause more than **transient** damage (a quarantined asset surfaces to the user; a rejected write returns a clear error; a divergence is detected and reconciled). The recovery paths in [Cryptography — Failure Modes](/design/cryptography/failure-modes/) cover key loss; this doc covers the *write-path* harm a wrong-but-signed client can attempt.

The threat model is not a primitives doc. Every primitive Capsule uses is declared in its [owner doc](/design/principles/#single-source-of-truth); this doc references those declarations rather than re-stating them. Where a specific invariant lives, the relevant owner doc enforces it; where a *defense* spans multiple docs, the canonical statement lives in one of the sub-docs below.

The cross-cutting invariants here are enforced by code that lives across many crates: `capsule-core::crypto::verify_asset` (client-side validation chokepoint), `capsule-api` (server-side envelope checks at every write path), and the [validation](/design/threat-model/validation/) sub-doc's invariants directly map to acceptance tests in the corresponding API crates.

## Sub-docs

| Sub-doc                                            | Concern                                                                                          |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| [Scenarios](/design/threat-model/scenarios/)       | Damage scenario → invariant map, the quarantine surface inventory, provenance immutability rules |
| [Validation](/design/threat-model/validation/)     | Server- + client-side refuse-by-default checklists; protocol handshake; idempotency; atomicity   |
| [Schema Rules](/design/threat-model/schema-rules/) | Schema evolution rules, forbidden client behaviors, deprecation policy, open questions           |

## Client Class Taxonomy

Every client request can be classified by one of these models. The defenses listed below apply to **all** of them — none of them are trusted to enforce their own correctness:

| Class         | Description                                                                                                                                                                  | What authenticates them                                                       | What stops them                                                                                                                                                                                                          |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Honest**    | Conforming implementation, correct keys, correct version.                                                                                                                    | Session token + access token + device DSK + epoch write-tier signature.       | Nothing to stop. This is the baseline.                                                                                                                                                                                   |
| **Faulty**    | Conforming intent, buggy implementation. Writes structurally invalid or semantically wrong manifests under real keys.                                                        | Same as honest — the keys are correct.                                        | Server-side [structural validation](/design/threat-model/validation/#server-side-validation-invariants) + client-side [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint + quarantine surfaces. |
| **Malicious** | Adversary in possession of a current device's DSK and the album's epoch write-tier key. Writes deliberately malformed or destructive operations.                             | Same as honest — the keys are real, because the adversary owns them.          | Provenance chain immutability + soft-delete window + per-album/per-event compartmentalization + audit trail for after-the-fact attribution.                                                                              |
| **Old**       | A signed-in client that predates a feature, schema, or suite the server now considers minimum. Cannot produce structurally valid writes for albums pinned above its version. | Authenticated, but `X-Capsule-Protocol` is below the server's accepted range. | [Protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation) rejects writes with `426 Upgrade Required` before any state is written.                                                       |
| **New**       | A prototype or staging build that writes a `protocol_version`/`crypto_suite_id`/`sidecar_schema` ahead of what the receiver knows.                                           | Authenticated, but the version is higher than the receiver's max known.       | Receiver's refuse-by-default rule on unknown enum values, unknown schemas, and forward-jumping protocol versions; closed schema evolution boundary (see [Schema Rules](/design/threat-model/schema-rules/)).             |

The deliberate choice in the matrix above: a *malicious* client with real keys is the hardest to stop, because confidentiality and authentication don't help when the adversary already holds the keys. Capsule's response is to ensure such an adversary can do nothing **silently** — every write produces a signed provenance record, soft-delete is the default, and history is append-only. The audit trail is the recovery surface.

## Damage Containment Layers

Restating the boundary hierarchy from [Core Principles](/design/principles/) as concentric containment shells, with the owner doc that enforces each:

| Shell                     | Boundary                                                         | Owner doc                                                                                                                  |
| ------------------------- | ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| **Per-version**           | Album protocol pinning isolates a buggy v_k from v_{k-1} albums. | [Versioning](/design/versioning/#album-protocol-version-pinning)                                                           |
| **Per-album**             | MLS group + per-epoch AMK + per-epoch write-tier key.            | [Cryptography — MLS](/design/cryptography/mls/) + [Cryptography — Keys](/design/cryptography/keys/#album-master-keys-amks) |
| **Per-event** (manifest)  | Each lifecycle action is its own signed, chained record.         | [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)                          |
| **Per-user**              | Owner Group Key, sponsored-account isolation.                    | [Cryptography — Keys](/design/cryptography/keys/#owner-group-keys-ogks)                                                    |
| **Per-peer** (federation) | Capability tokens, error budgets, quarantine for new peers.      | [Federation](/design/federation/)                                                                                          |
| **Per-device** (peering)  | Device directory enforced via the TLS handshake.                 | [Peering — Establishing the Channel](/design/peering/#establishing-the-channel)                                            |

A bug or compromise on one side of any shell cannot cross it.

## Owner Doc Cross-Reference

Each owner doc gains a short section linking back to the relevant threat-model invariant. The mapping (for navigation):

| Owner doc                                     | Threat-model section(s) it ties into                                                                                                                                                                                                    |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Principles](/design/principles/)             | [§ Damage Containment Layers](#damage-containment-layers)                                                                                                                                                                               |
| [Versioning](/design/versioning/)             | [Protocol Negotiation](/design/threat-model/validation/#protocol-and-capability-negotiation), [Atomicity](/design/threat-model/validation/#atomicity-invariants)                                                                        |
| [Filesystem](/design/filesystem/)             | [Server Validation](/design/threat-model/validation/#server-side-validation-invariants), [Atomicity](/design/threat-model/validation/#atomicity-invariants), [Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces) |
| [Cryptography](/design/cryptography/)         | [Provenance Immutability](/design/threat-model/scenarios/#provenance-immutability-rules), [Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) (signature/chain rows)                                         |
| [Metadata](/design/metadata/)                 | [Schema Evolution](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar), [Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) (CRDT rows)                                                   |
| [Import & Synchronization](/design/import/)   | [Server Validation](/design/threat-model/validation/#server-side-validation-invariants), [Idempotency](/design/threat-model/validation/#idempotency-invariants)                                                                         |
| [Federation](/design/federation/)             | [Server Validation](/design/threat-model/validation/#server-side-validation-invariants), [Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces)                                                                     |
| [Peering](/design/peering/)                   | [Client Validation](/design/threat-model/validation/#client-side-validation-invariants), [Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) (peer rows)                                                     |
| [Authentication](/design/authentication/)     | [Forbidden Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors), [Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) (revoke-all row)                                                   |
| [Authorization](/design/authorization/)       | [Server Validation](/design/threat-model/validation/#server-side-validation-invariants)                                                                                                                                                 |
| [Backup & Recovery](/design/backup-recovery/) | [Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces)                                                                                                                                                              |
| [Thumbnails](/design/thumbnails/)             | [Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) (derivative row)                                                                                                                                         |
| [AI/ML Integrations](/design/ai/)             | [Forbidden Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors) (AI tag namespace); [Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map) (embedding model row)                            |
| [Organization](/design/organization/)         | [Atomicity](/design/threat-model/validation/#atomicity-invariants), [Forbidden Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors)                                                                                |
| [Clients](/design/clients/)                   | [Client Validation](/design/threat-model/validation/#client-side-validation-invariants), [Deprecation Policy](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy)                                               |
