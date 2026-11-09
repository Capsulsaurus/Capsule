---
title: Core Principles
description: The core principles that guide the design and development of Capsule
status: draft
---

These principles bind every Capsule component, client and server. The rules after them bind every
doc in `design/`.

## Principles

- **Determinism and idempotency.** Raw original data is the source of truth. Every process is repeatable from its inputs.
- **4 KiB alignment.** Data crossing disk or memory boundaries is processed and written 4 KiB-aligned unless a concrete case demands otherwise.
- **Forward and backward compatibility.** Old clients ignore new fields and new clients tolerate missing ones, subject to the [asymmetry](#postels-law-asymmetric) below.
- **Data integrity.** Capsule never deletes data unexpectedly. It acts only in strict scenarios and otherwise crashes early. Server data is assumed durable; client data is assumed losable.
- **Ephemeral derived data.** Anything that is not an original asset is rebuildable and treated as such.
- **Encryption and compartmentalization.** Metadata is encrypted alongside data. Every boundary (album, peer, event, user, version) is a failure-containment boundary.
- **Offline/online divide.** A feature works either solely online or solely offline, never differently by connectivity.
- **Recovery-first.** The filesystem is reconstructible from partial corruption. Sidecars are the canonical metadata store; the database is a rebuildable query cache.
- **Self-describing.** Each media file is paired with a CBOR sidecar holding all user-editable and stable metadata, interpretable without a database.
- **Atomic writes.** Temp file and rename everywhere. Never overwrite in place.

### Postel's Law (asymmetric)

Within a known schema version Capsule is liberal: unknown sidecar fields are preserved verbatim and missing optional fields are tolerated. **Across versions it is closed.** A structure announcing a schema version (`sidecar_schema`, `crypto_suite_id`, `protocol_version`) above the receiver's maximum is rejected, never best-effort parsed. See [](/design/threat-model/schema-rules/).

## Single Source of Truth

Every primitive, construction, format or component identity is **declared in exactly one owner doc**. Other docs link the declaration and never restate the choice. Explaining a behaviour elsewhere ("STREAM tags catch chunk reordering") is fine; restating the choice ("we use SHA-256") is not. When in doubt, link.

| Domain | Owner doc |
| --- | --- |
| Cryptographic primitives, constructions, version identifiers | [](/design/cryptography/primitives/) |
| Key hierarchy, device coordination | [](/design/cryptography/keys/) |
| MLS membership, ciphersuite binding | [](/design/cryptography/mls/) |
| Asset + metadata encryption | [](/design/cryptography/encryption/) |
| Provenance chains, signed manifests | [](/design/cryptography/provenance/) |
| MLS resilience | [](/design/mls-resilience/) |
| Device enrollment | [](/design/device-enrollment/) |
| ML model identities, embedding provenance | [](/design/ai/) |
| LQIP, thumbnail/preview formats | [](/design/thumbnails/) |
| Server filesystem, required services | [](/design/filesystem/server/) |
| Client filesystem, local index | [](/design/filesystem/client/) |
| Library self-maintenance, atomic-write granularity | [](/design/filesystem/maintenance/) |
| Tokens, identity binding, auth flow | [](/design/authentication/) |
| Backup container, escrow, recovery paths | [](/design/backup-recovery/) |
| CRDTs, identifiers, geolocation, sidecar schema | [](/design/metadata/) |
| Import pipeline | [](/design/import/pipeline/) |
| Upload protocol | [](/design/transfer/upload/) |
| Download, sync feed, auto-sync | [](/design/transfer/download/) |
| Storage durability, verify-before-destroy | [](/design/transfer/durability/) |
| Tethered camera import | [](/design/import/camera-import/) |
| Connection and retry classes, transport security | [](/design/networking/) |
| Offline-first requirements | [](/design/local-gallery/) |
| Federation trust, capability tokens | [](/design/federation/) |
| LAN discovery, peer channel | [](/design/peering/) |
| Album protocol version pinning | [](/design/versioning/) |
| Stacking, trash retention | [](/design/organization/) |
| Lifecycle action set | [](/design/authorization/) |
| Damage scenarios, client classes | [](/design/threat-model/) |
| Refuse-by-default validation invariants | [](/design/threat-model/validation/) |
| Schema evolution, deprecation | [](/design/threat-model/schema-rules/) |
| Outbound licence, dependency-licence policy | [](/design/licensing/) |
| Share links | [](/design/share-links/) |
| Upload links, guest drops | [](/design/web-upload/) |
| Moderation, reporting, blocklists | [](/design/moderation/) |
| Quota accounting + enforcement | [](/design/quota/) |
| Client validation duties, sandboxed decoder | [](/design/clients/) |
| Alerts, delivery, wake tier | [](/design/notifications/) |
| Catalogs, locale resolution, error codes | [](/design/i18n/) |
| API surface ↔ transport map | [](/design/api-surfaces/) |
| Developer-reference generation | [](/design/developer-docs/) |

## Doc Structure

Design docs are not templated, but each must answer four questions: **where it lives** (crate or module), **what its public surface is** (schema, wire format, trait, error domain), **what it defers to** (owner-doc links) and **how it is validated** (a [tier](#validation-tiers), where non-obvious). The code is the source of truth for module layout. Docs state contracts, not restated code.

### Implementation Status

A doc naming an implementing module states per surface whether it is **Implemented** (module exists, its tier runs in CI), **Planned** (contract designed, no code) or **Blocked** (planned, with a named external blocker). A planned contract is written in normative present tense but must not claim the code exists. Every module a doc names that does not exist yet must be listed in `capsule-docs/planned-modules.txt`.

### Review Status

Every page carries a required `status` frontmatter field tracking human review: `draft` (not re-reviewed since the last substantive change) or `stable` (signed off; a substantive edit flips it back). Drafts still publish, and the docs site badges every non-stable page in its navigation ([](/development/visual-language/#docs-site)).

## Validation Tiers

The tier is decided by **what is mocked**, not by where the test lives.

- **Unit:** in-module tests against the contract, with peers and externals mocked. Deterministic and fast.
- **Smoke:** one module's real implementation with real I/O and backing services (e.g. testcontainers Postgres or Valkey), peers mocked.
- **E2E:** several modules wired against real infrastructure. The list is **bounded** in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs); growth past it signals unwanted coupling.

## Damage Containment

A faulty, malicious or version-mismatched client must not inflict irreparable damage on user data. These principles name the posture and the [](/design/threat-model/) owns the defenses: the client-class taxonomy, the damage-scenario-to-invariant map, validation invariants, fail-closed protocol negotiation, and the cross-cutting idempotency, atomicity and quarantine rules.
