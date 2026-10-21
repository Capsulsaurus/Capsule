---
title: Core Principles
description: The core principles that guide the design and development of Capsule
status: draft
---

These principles apply to every Capsule component, client and server, and the rules below apply to
every doc in `design/`.

## Principles

- **Determinism + idempotency.** Raw original data is the source of truth; every process is repeatable from its inputs.
- **4 KiB alignment.** Data touching disk or memory boundaries is processed and written 4 KiB-aligned unless a concrete case demands otherwise.
- **Forward and backward compatibility.** Old clients ignore new fields; new clients tolerate missing ones — subject to the [asymmetry](#postels-law-asymmetric) below.
- **Data integrity.** Capsule never deletes data unexpectedly: act only in strict scenarios, otherwise crash early. Server data is assumed durable; client data is assumed losable.
- **Ephemeral derived data.** Anything that is not an original asset is rebuildable and treated as such.
- **Encryption + compartmentalization.** Metadata is encrypted alongside data. Every boundary (album, peer, event, user, version) is a failure-containment boundary.
- **Offline/online divide.** A feature works either solely online or solely offline, never differently by connectivity.
- **Recovery-first.** The filesystem is reconstructible from partial corruption. Sidecars are the canonical metadata store; the database is a rebuildable query cache.
- **Self-describing.** Each media file is paired with a CBOR sidecar holding all user-editable and stable metadata, interpretable without a database.
- **Atomic writes.** Temp-file + rename everywhere; never overwrite in place.

### Postel's Law (asymmetric)

Liberal *within a known schema version*: unknown sidecar fields are preserved verbatim and missing optional fields tolerated. **Cross-version is closed**: a structure announcing a schema version (`sidecar_schema`, `crypto_suite_id`, `protocol_version`) above the receiver's maximum is rejected, never best-effort-parsed. See [Threat Model — Schema Rules](/design/threat-model/schema-rules/).

## Single Source of Truth

Every primitive, construction, format, or component identity is **declared in exactly one owner doc**; other docs link the declaration and never restate the choice. Explaining a behaviour elsewhere ("STREAM tags catch chunk reordering") is fine; restating the choice ("we use SHA-256") is not. When in doubt, link.

| Domain | Owner doc |
| --- | --- |
| Cryptographic primitives, constructions, version identifiers | [Cryptography — Primitives](/design/cryptography/primitives/) |
| Key hierarchy, device coordination | [Cryptography — Keys](/design/cryptography/keys/) |
| MLS membership, ciphersuite binding | [Cryptography — MLS](/design/cryptography/mls/) |
| Asset + metadata encryption | [Cryptography — Encryption](/design/cryptography/encryption/) |
| Provenance chains, signed manifests | [Cryptography — Provenance](/design/cryptography/provenance/) |
| Recovery paths, failure modes, transport security | [Cryptography — Failure Modes](/design/cryptography/failure-modes/) |
| MLS resilience | [MLS Resilience](/design/mls-resilience/) |
| Device enrollment | [Device Enrollment](/design/device-enrollment/) |
| ML model identities, embedding provenance | [AI/ML Integrations](/design/ai/) |
| LQIP, thumbnail/preview formats | [Thumbnails and Previews](/design/thumbnails/) |
| Server filesystem, required services | [Filesystem — Server](/design/filesystem/server/) |
| Client filesystem, local index | [Filesystem — Client](/design/filesystem/client/) |
| Library self-maintenance, atomic-write granularity | [Filesystem — Maintenance](/design/filesystem/maintenance/) |
| Tokens, identity binding, auth flow | [Authentication](/design/authentication/) |
| Backup container, escrow, recovery | [Backup and Recovery](/design/backup-recovery/) |
| CRDTs, identifiers, geolocation, sidecar schema | [Metadata](/design/metadata/) |
| Import pipeline | [Import — Pipeline](/design/import/pipeline/) |
| Upload protocol | [Transfer — Upload](/design/transfer/upload/) |
| Download, sync feed, auto-sync | [Transfer — Download](/design/transfer/download/) |
| Storage durability, verify-before-destroy | [Transfer — Durability](/design/transfer/durability/) |
| Tethered camera import | [Import — Camera Import](/design/import/camera-import/) |
| Connection and retry classes | [Network Resilience](/design/networking/) |
| Offline-first requirements | [Local Gallery](/design/local-gallery/) |
| Federation trust, capability tokens | [Federation](/design/federation/) |
| LAN discovery, peer channel | [Peering](/design/peering/) |
| Album protocol version pinning | [Versioning](/design/versioning/) |
| Stacking, trash retention | [Asset Organization](/design/organization/) |
| Lifecycle action set | [Authorization](/design/authorization/) |
| Damage scenarios, client classes | [Threat Model](/design/threat-model/) |
| Refuse-by-default validation invariants | [Threat Model — Validation](/design/threat-model/validation/) |
| Schema evolution, deprecation | [Threat Model — Schema Rules](/design/threat-model/schema-rules/) |
| Outbound licence, dependency-licence policy | [Licensing](/design/licensing/) |
| Dependency and tooling pins | [Dependencies](/design/dependencies/) |
| Share links | [Share Links](/design/share-links/) |
| Upload links, guest drops | [Web Upload](/design/web-upload/) |
| Moderation, reporting, blocklists | [Moderation](/design/moderation/) |
| Quota accounting + enforcement | [Quota](/design/quota/) |
| Client validation duties, sandboxed decoder | [Clients](/design/clients/) |
| Alerts, delivery, wake tier | [Notifications](/design/notifications/) |
| Catalogs, locale resolution, error codes | [Internationalization](/design/i18n/) |
| Crate graph, bounded E2E surface | [Module Map](/design/module-map/) |
| API surface ↔ transport map | [API Surfaces](/design/api-surfaces/) |
| Developer-reference generation | [Developer Documentation](/design/developer-docs/) |

## Doc Structure

Design docs are not templated, but each must let a reader answer: **where does this live** (crate or module), **what is its public surface** (schema, wire format, trait, error domain), **what does it defer to** (owner-doc links), and **how is it validated** (a [tier](#validation-tiers), where non-obvious). The code is the source of truth for module layout; docs state contracts, not restated code.

### Implementation Status

A doc naming an implementing module states per surface whether it is **Implemented** (module exists, its tier runs in CI), **Planned** (contract designed, no code), or **Blocked** (planned, with a named external blocker). A planned contract is written in normative present tense but must not claim the code exists; a module a doc names that does not exist yet must be listed in `capsule-docs/planned-modules.txt`.

### Review Status

Every page carries a required `status` frontmatter field tracking human review: `draft` (not re-reviewed since the last substantive change) or `stable` (signed off; substantive edits flip it back). Drafts still publish.

## Validation Tiers

- **Unit** — in-module tests against the contract, peers and externals mocked. Deterministic and fast.
- **Smoke** — one module's real implementation with real I/O and backing services (e.g. testcontainers Postgres or Valkey), peers mocked.
- **E2E** — several modules wired against real infrastructure. The list is **bounded** in [Module Map — E2E Test Surface](/design/module-map/#e2e-test-surface); growth past it signals unwanted coupling.

The tier is decided by **what is mocked**, not where the test lives.

## Damage Containment

A faulty, malicious, or version-mismatched client must not inflict irreparable damage on user data. These principles name the posture; the [Threat Model](/design/threat-model/) owns the defenses: the client-class taxonomy, the damage-scenario → invariant map, validation invariants, fail-closed protocol negotiation, and the cross-cutting idempotency, atomicity, and quarantine rules.
