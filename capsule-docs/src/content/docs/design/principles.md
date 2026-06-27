---
title: Core Principles
description: The core principles that guide the design and development of Capsule
---

These principles apply universally to all components of Capsule, from clients to server. The owner-doc rules and structural guidance below apply to every doc in `design/`.

## Principles

- **Determinism + idempotency.** Raw original data is the source of truth; every process is repeatable from its inputs.
- **4 KiB alignment.** Data is processed and written 4 KiB-aligned where it touches disk or memory boundaries. Smaller or larger multiples are introduced only when a concrete edge case demands it.
- **Forward and backward compatibility.** Old clients ignore new fields; new clients tolerate missing ones gracefully — subject to the [Postel's Law asymmetry](#postels-law-asymmetric) below.
- **Data integrity.** Capsule never deletes data unexpectedly. Act under strict scenarios; otherwise crash early. Multiple layers of safeguards guard against current and future bugs. Server data is assumed durable (robust hardware); client data is assumed potentially lost.
- **Ephemeral derived data.** Anything that isn't an original asset can be rebuilt and is treated as rebuildable.
- **Encryption + compartmentalization.** Sensitive code and storage stay separated. Metadata is encrypted alongside data. Every boundary — per-album, per-peer, per-event, per-user, per-version — is a failure-containment boundary; a bug or compromise on one side cannot cross.
- **Offline/online divide.** A feature works either solely online or solely offline, not differently by connectivity. This simplifies business logic and limits state-shift risk.
- **Recovery-first.** The filesystem must be reconstructible from partial corruption. No database is required to interpret critical data — sidecar files are the canonical metadata store; the database is a rebuildable query cache.
- **Self-describing.** Each media file is paired with a CBOR sidecar containing all user-editable and stable metadata. Files are independently interpretable without a running database.
- **Atomic writes.** Use temp-file + rename throughout. Direct overwrites risk corruption on power loss.

### Postel's Law (asymmetric)

Liberal in what we accept *within a known schema version* — unknown sidecar fields are preserved verbatim and missing optional fields are tolerated. **Cross-version is closed**: a structure announcing a schema version (`sidecar_schema`, `crypto_suite_id`, `protocol_version`) above the receiver's max known is rejected, never best-effort-parsed. The asymmetry is what prevents a faulty or new client from silently corrupting state — see [Threat Model — Schema Rules](/design/threat-model/schema-rules/).

## Single Source of Truth

Every primitive, construction, format, or component identity Capsule depends on is **declared in exactly one design doc**. Other docs reference the declaration by anchor; they never restate the choice. The goal is that swapping a primitive (a hash, a model, a container format) is a single-doc edit, not a 10-doc cascade that silently leaves inconsistencies.

The owner docs are:

| Domain                                                                | Owner doc                                                           |
| --------------------------------------------------------------------- | ------------------------------------------------------------------- |
| All cryptographic primitives + constructions + versioning identifiers | [Cryptography — Primitives](/design/cryptography/primitives/)       |
| Cryptographic key hierarchy + device coordination                     | [Cryptography — Keys](/design/cryptography/keys/)                   |
| MLS group membership + ciphersuite binding                            | [Cryptography — MLS](/design/cryptography/mls/)                     |
| Asset + metadata encryption                                           | [Cryptography — Encryption](/design/cryptography/encryption/)       |
| Provenance chains + signed manifests + derivative provenance          | [Cryptography — Provenance](/design/cryptography/provenance/)       |
| Recovery paths + failure-mode catalog + transport security            | [Cryptography — Failure Modes](/design/cryptography/failure-modes/) |
| MLS resilience (state divergence, lost commits, re-keying)            | [MLS Resilience](/design/mls-resilience/)                           |
| Device enrollment + cross-device add ceremony                         | [Device Enrollment](/design/device-enrollment/)                     |
| ML model identities + embedding provenance                            | [AI/ML Integrations](/design/ai/)                                   |
| LQIP scheme + thumbnail/preview formats                               | [Thumbnails and Previews](/design/thumbnails/)                      |
| Server filesystem (blob store, Postgres index, deployment profiles)   | [Filesystem — Server](/design/filesystem/server/)                   |
| Client filesystem (library layout, local index, space recovery)       | [Filesystem — Client](/design/filesystem/client/)                   |
| Library self-maintenance + atomic-write granularity                   | [Filesystem — Maintenance](/design/filesystem/maintenance/)         |
| Session/access tokens + identity binding + auth flow                  | [Authentication](/design/authentication/)                           |
| Backup artifact container + escrow + recovery mechanisms              | [Backup and Recovery](/design/backup-recovery/)                     |
| CRDT scheme, identifiers, geolocation, sidecar schema                 | [Metadata](/design/metadata/)                                       |
| Import pipeline (scan, plan, execute)                                 | [Import — Pipeline](/design/import/pipeline/)                       |
| Upload protocol (wire, sessions, finalization)                        | [Import — Upload Protocol](/design/import/upload-protocol/)         |
| Download, sync feed, tiered fetch, auto-sync                          | [Import — Download & Sync](/design/import/download-sync/)           |
| Federation trust model, capability tokens, soft-fail                  | [Federation](/design/federation/)                                   |
| LAN discovery + peer channel + delta transfer                         | [Peering](/design/peering/)                                         |
| Album protocol version pinning + upgrade ceremony                     | [Versioning](/design/versioning/)                                   |
| Stacking taxonomy + trash retention semantics                         | [Asset Organization](/design/organization/)                         |
| Lifecycle action set                                                  | [Authorization](/design/authorization/)                             |
| Damage scenarios + client class taxonomy + containment shells         | [Threat Model](/design/threat-model/)                               |
| Server- + client-side refuse-by-default validation invariants         | [Threat Model — Validation](/design/threat-model/validation/)       |
| Schema evolution rules, forbidden behaviors, deprecation policy       | [Threat Model — Schema Rules](/design/threat-model/schema-rules/)   |
| Share links + public-share serving                                    | [Share Links](/design/share-links/)                                 |
| Web upload — upload links, guest drops, adoption                      | [Web Upload](/design/web-upload/)                                   |
| Moderation policy + federated reporting + blocklists                  | [Moderation](/design/moderation/)                                   |
| Quota accounting + enforcement points                                 | [Quota](/design/quota/)                                             |
| Client validation duties + sandboxed decoder                          | [Clients](/design/clients/)                                         |
| Translation catalog format + locale resolution + error-code scheme    | [Internationalization](/design/i18n/)                               |
| Code module → design doc mapping + bounded E2E test surface           | [Module Map](/design/module-map/)                                   |

**Permitted secondary mentions.** Mechanism-explanatory phrasing inside a non-owner doc is fine — for example, "STREAM tags catch chunk reordering" inside [Peering](/design/peering/) is explaining a *behavior*, not declaring a *choice*. What the rule forbids is restating the choice itself ("we use SHA-256") outside the owner doc. When in doubt, link.

## Doc Structure

Design docs are **not templated**. Each doc's structure is chosen to fit its content — a wire-protocol doc is naturally state-machine-shaped, a primitives inventory is naturally a table, the threat-model scenario doc is naturally a matrix. What stays consistent is *what every doc must make legible*, not *how it must look*.

Regardless of shape, every design doc must let a reader answer four questions:

1. **Where does this live?** — Which crate(s) / module(s) implement this. Surface this however reads best: a header callout, an intro sentence, or per-section "implemented in…" notes. Don't bolt on a labeled "Module Boundary" section if it doesn't add clarity.
2. **What is its public surface?** — The contract other modules depend on: schema, wire format, trait shape, manifest envelope, closed enum, error domain. For some docs the entire doc *is* the surface (a CBOR schema doc, a primitives inventory); for others the surface is one focused subsection. Promote it visually only if a reader couldn't otherwise locate it quickly.
3. **What does it own vs. defer to?** — Owner-anchor links to the SSoT for upstream primitives. This is the [SSoT rule](#single-source-of-truth) applied per doc.
4. **How is it validated?** — Brief tier notes (see [Validation Tiers](#validation-tiers) below) where the answer is non-obvious or where the cross-module test surface needs bounding. For docs whose validation collapses entirely to "the threat-model scenario map enforces this," a one-line pointer is enough.

These are goals, not required headings. Some docs hit all four in a single intro paragraph; others (especially wire-protocol and schema docs) dedicate focused sections. The choice is per doc.

The [Module Map](/design/module-map/) is the cross-cutting index: every code module → owning design doc → validation tier. It is the developer's first stop.

## Validation Tiers

The three test tiers a design doc may reference:

- **Unit** — In-module tests against the doc's contract surface, with peer modules and external dependencies mocked. Deterministic, fast, run on every change. Example: signing and verifying a manifest against fixed test vectors inside `capsule-core::crypto`.
- **Smoke** — Single-module end-to-end with the module's real implementation but its peers mocked. Uses real I/O and real backing services (e.g. testcontainers for Postgres or Valkey). Example: the upload-server full session lifecycle (`POST /upload` → `PATCH` → finalization) against a real Postgres, with no client process — the client side is mocked at the HTTP boundary.
- **E2E** — Multiple modules wired together against real infrastructure. The list is **bounded** in [Module Map — E2E Test Surface](/design/module-map/#e2e-test-surface). Any addition requires updating that list — E2E surface growing past the bound is a signal the design has unwanted coupling worth examining.

The split is enforced by **what is mocked, not by location in the source tree**. A test under `crate/tests/integration/` that mocks every peer is still a unit test for the purposes of this taxonomy.

## Damage Containment

A faulty, malicious, or version-mismatched client must not be able to inflict irreparable damage on user data. The principles above (data integrity, atomic writes, recovery-first, self-describing, Postel's Law, encryption + compartmentalization) name the *posture*; the [Threat Model](/design/threat-model/) names the *defenses*.

In particular, the threat model owns:

- The **client class taxonomy** (honest, faulty, malicious, old, new, and the web-guest uploader) — how each is authenticated and what stops each from doing harm.
- The **damage scenario → invariant map** — for every concrete attack or bug class, the single owner doc that defeats it.
- **Server- and client-side validation invariants** — the refuse-by-default structural checks a key-less server and every client run on every write.
- **Protocol and capability negotiation** — the universal fail-closed handshake that rejects version mismatches before any state is written.
- **Idempotency, atomicity, and quarantine** rules that span owner docs.

Each owner doc grows a short section pointing into the relevant threat-model section, but the cross-cutting statements live there. Principles continues to own the universal *posture*; threat model owns the universal *defenses*.
