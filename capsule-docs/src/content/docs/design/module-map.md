---
title: Module Map
description: Current Rust modules, planned boundaries, and validation ownership
status: draft
---

This map distinguishes code that is active today from contracts that are planned or preserved only
for review. A name in the design does not imply that a deployable implementation exists.

## Current Status

| Area | Status | Ownership |
| --- | --- | --- |
| `capsule-core` | Active | Cryptography (including the live MLS album authority), canonical CBOR, validation, CRDTs, sidecars, backup, lifecycle, client filesystem, local SQLite and vector index, import scan/plan, culling, share and drop crypto, aggregated federation views, ML orchestration |
| `capsule-wasm` | Active | The browser sealing surface `capsule-web` loads — share-link open and guest drop sealing over `capsule-core` with default features off. Built by `mise run build-wasm`; never committed |
| `capsule-i18n` + `xtask::i18n` | Active | Canonical ICU catalogs, runtime localization, generated platform catalogs |
| `capsule-core-ffi` | Active | UniFFI bindings for native Swift/Kotlin consumers, consolidated on one UniFFI version across both surfaces |
| `capsule-cli` + CLI entity/migration crates | Active | Local CLI behavior and its SQLite persistence. Its network commands (`auth`, `sync`, `list`, `push`) ride the Rust SDK and pause while that is rebuilt |
| Client import execution | Quarantined | Rebuild over Rawshift plus direct Chromahash v1; scan, grouping, and planning remain active. The signed-path executor is a partial replacement — it already applies privacy and sidecar policy before encrypting, signing, and committing, but still consumes in-repository media rather than normalized Rawshift results |
| Server | Quarantined | Rebuild with Kynos as REST/OpenAPI-only `capsule-api` modules |
| Rust SDK | Quarantined | Regenerate with Spargen from the canonical Kynos OpenAPI document. Orchestration (auth/session refresh, upload, sync, recovery, protocol-version negotiation) is Capsule-owned and stays outside generated code |
| Media pipeline | Quarantined | Rawshift replaces in-repository codecs and metadata extraction |
| GraphQL and gRPC transports | Deleted | Rejected; no compatibility surface will be restored |

Review-only sources live under `legacy-review/`. They are not Cargo packages and have no validation
status until rewritten against their owning contracts.

## Active `capsule-core` Ownership

| Module | Owning design | Validation |
| --- | --- | --- |
| `crypto::{primitives,keys,encryption,provenance,verify_asset}` | [Cryptography](/design/cryptography/) and [Authorization](/design/authorization/) | Unit vectors, negative cases, smoke |
| `crypto::authority` | [Keys — Write Authority Interface](/design/cryptography/keys/) | Unit; epoch-ledger round trip |
| `crypto::authority::openmls_authority` | [MLS](/design/cryptography/mls/) and [MLS Resilience](/design/mls-resilience/) | Unit and smoke; protocol round trip |
| `cbor` | [Metadata](/design/metadata/) | Canonical-byte vectors; cross-language conformance |
| `validation` | [Validation](/design/threat-model/validation/) | Pure invariant unit tests |
| `backup` | [Backup and Recovery](/design/backup-recovery/) | Unit and smoke |
| `lifecycle` | [Organization](/design/organization/) and [Provenance](/design/cryptography/provenance/) | Unit and smoke; signed write path |
| `library::{init,open,rebuild,scrub,trash,cache,lock,receipts,auth_gate}` | [Client Filesystem](/design/filesystem/client/) and [Maintenance](/design/filesystem/maintenance/) | Unit and smoke |
| `library::{space,storage_verify}` | [Import Pipeline](/design/import/pipeline/) and [Storage Verification](/design/import/storage-verification/) | Unit boundary and release-gate tests |
| `import::{scanner,planner,group,special,plan,importers,streaming,upload}` | [Import Pipeline](/design/import/pipeline/) | Unit; executor smoke is blocked on Rawshift |
| `drop`, `sharing` | [Web Upload](/design/web-upload/) and [Share Links](/design/share-links/) | Unit and KAT; sealing round trips cross-language against `capsule-wasm` |
| `culling` | [Organization — Culling](/design/organization/) | Unit; filtered views and reject sweep |
| `federation` | [Federation](/design/federation/) | Unit; aggregated album view over authority fixtures |
| `ml::{registry,orchestrator,regen,runner}` | [AI](/design/ai/) | Unit determinism; the real inference runner is post-v1 |
| `cohort` | [Authentication](/design/authentication/) | Unit determinism |
| `metadata`, `sidecar` | [Metadata](/design/metadata/) | Unit determinism and round trips |
| `db` | [Client Filesystem](/design/filesystem/client/) | Unit SQLite operations; vector index over `sqlite-vec` |
| `domain`, `models` | [Organization](/design/organization/), [Metadata](/design/metadata/) | Closed-enum and model unit tests |

OpenMLS and the inference engines are implementation dependencies; Capsule retains the application
protocols, schemas, provenance, and policy. Peering lives in the SDK and is quarantined with it.

## Planned Server Modules

The future `capsule-api` is one Kynos REST/OpenAPI application composed from cohesive internal
modules, not separate public transports or microservices.

| Module | Contract owner | Required validation |
| --- | --- | --- |
| `auth` | [Authentication](/design/authentication/) and [Device Enrollment](/design/device-enrollment/) | Unit plus Postgres/Valkey adapter parity |
| `upload` | [Upload Protocol](/design/import/upload-protocol/) | State-machine property tests, adapter parity, smoke and E2E |
| `blob` | [Server Filesystem](/design/filesystem/server/) and [Storage Verification](/design/import/storage-verification/) | Layout, range, corruption, crash, quarantine and GC tests |
| `sync` | [Download and Sync](/design/import/download-sync/) | Cursor, monotonicity, pagination and range-resume tests |
| `shares` | [Share Links](/design/share-links/) | Capability and expiry tests |
| `federation` | [Federation](/design/federation/) | Capability, compartmentalization and pull-path tests |
| `quota`, `moderation` | [Quota](/design/quota/) and [Moderation](/design/moderation/) | Unit plus policy smoke tests |

The server owns its content-addressed blob implementation behind a Capsule-defined backend trait.
The E2EE-aware resumable protocol also stays in Capsule. Authentication state and upload state use
separate typed ports; no generic CAS, transfer, or TTL library is planned.

## Planned Client Boundaries

| Boundary | Decision |
| --- | --- |
| REST client | Spargen-generated Rust from a checked-in OpenAPI 3.1 document |
| SDK workflows | Capsule-owned authentication, upload, sync, recovery, and protocol-version orchestration |
| Workspace verbs over FFI | The `capsule_sdk` UniFFI namespace exposes the workspace surface apps need — enroll/open (including a hardware-signer constructor), albums, seal and import, verify, sync-apply, master-key escrow, and device-directory publish. Orchestration and shape only: each verb is one call into `capsule-core`, which keeps every cryptographic step, and the `capsule_core` namespace never shares a binary with it |
| Media | Rawshift performs detection, decode/encode, metadata normalization, derivatives, previews, and video work |
| LQIP | Capsule imports Chromahash directly after v1; Rawshift has no Chromahash responsibility |
| Import commit | Capsule applies privacy policy, creates sidecars/provenance, encrypts, signs, and commits normalized media results |

## External Dependency Register

These are the intended complexity boundaries. A dependency is not added to an active manifest until
the named acceptance gaps are verified with contract fixtures or a minimal spike.

| Library | Scope Capsule delegates | Acceptance gaps Capsule must verify |
| --- | --- | --- |
| Kynos | HTTP runtime, REST routing, middleware composition, OpenAPI 3.1 emission, limits, shutdown, observability | Streaming request/response bodies, cancellation and backpressure; deterministic schema output; custom protocol/error headers on every response; middleware ordering; test harnesses without live infrastructure |
| Spargen | Rust client generation from the checked-in Kynos OpenAPI contract | OpenAPI 3.1 compatibility; streaming upload/range download; opaque binary bodies; stable error-code mapping; auth and protocol headers; supported Rust targets; deterministic generation and version-compatibility checks |
| Rawshift | Media detection, decoding/encoding, metadata normalization, derivatives, previews, and video processing | Required format/codec matrix; bounded memory and concurrency; cancellation/progress; malformed-input isolation; deterministic orientation/color/HDR behavior; normalized metadata provenance; mobile/desktop targets; no Chromahash API |
| Chromahash v1 | LQIP encode/decode only, imported directly by Capsule | Stable v1 wire format and versioning; deterministic output; wide-gamut/HDR fixtures; decoder fallback behavior; supported FFI targets. It remains absent from Cargo until v1 is released |
| OpenMLS | MLS protocol and cryptographic state transitions | Required cipher suites and credential model; deterministic persistence/restore; external signer integration; epoch/exporter behavior; cross-platform size/performance; Capsule-owned album policy and provenance stay outside it |
| PostgreSQL driver/ORM | Durable server index and default implementations of the two typed state ports | Transactions needed for finalization, row locking, migration strategy, cancellation, typed error mapping, tracing, and adapter conformance. Select the narrowest mature stack after Kynos integration is proven |
| `redis-rs` | Required Valkey adapters for `AuthStateStore` and `UploadSessionStore` | Atomic compare/update and expiry primitives required by each port; cluster behavior; cancellation/timeouts; tracing; parity with PostgreSQL and in-memory test suites |
| RustCrypto, `ciborium`, `rusqlite`, `sqlite-vec`, UniFFI, `wasm-bindgen` | Existing crypto primitives, canonical serialization, local catalog and vector index, native bindings, and the browser boundary | Continue vectors, canonical-byte tests, migration tests, and binding smoke tests; these libraries do not own Capsule protocols or schemas |

Explicit non-dependencies: no generic CAS crate, `object_store`, resumable-transfer library, generic
TTL/CAS library, GraphQL/gRPC stack, or in-repository media codec stack. Reconsider extraction only
after a product-neutral interface has two real consumers and removes more audit surface than it adds.

## E2E Test Surface

The E2E surface is **bounded**: adding a test here means adding it to the relevant doc's Validation
section and justifying why the cross-module surface is irreducible. Each case must remain backed
primarily by unit and adapter-contract tests. Cases are numbered so that code can name the case it
covers (`rg "E2E case N"`), and slices in the repo-root `SLICES.md` reference these numbers.

**Status.** Every case with a server or SDK leg is **suspended** until the Kynos rebuild and the
regenerated Rust SDK land — the implementations that previously satisfied them are review-only
material under `legacy-review/`. Their `capsule-core` halves continue to ship and stay covered by
unit and smoke tests. Cases whose surface is entirely inside `capsule-core` are unaffected.

1. **Auth → sync → client-side library query.** Sign in → access token → the sync feed returns the
   account's album entries → the client applies them and a local `library.sqlite` query lists the
   expected albums (rich queries are client-side per [API Surfaces](/design/api-surfaces/)).
2. **Full import + upload + finalize.** Local scan → plan → execute → upload session → finalize →
   blob present at `blobs/{hash}` and the index row marked uploaded.
3. **Sync feed pickup.** Upload from device A → device B's feed advances → device B fetches the
   metadata blob and, per scope, the original.
4. **Federation cross-server pull.** Alice on `home.tld` shares to Bob on `other.tld` → capability
   token → Bob's server pulls metadata and blobs → Bob's client renders.
5. **LAN peering A→B.** Two devices on one LAN; discovery → TLS handshake → delta-scoped artifact →
   restore on the receiver → byte-equal libraries.
6. **Backup → restore on a fresh device.** Export a full backup → bootstrap a new device via
   passphrase and escrow → import the backup → assert every asset present and verifiable.
7. **Full lifecycle.** Create → metadata update → trash → restore → re-delete → hard purge after
   retention. The provenance chain advances through every transition and the server refuses purge
   before `retention_until`.
8. **Album upgrade ceremony.** Multi-member album; an admin initiates the upgrade → quiesce → drain
   → tombstone → fork → queued writes replay. Includes one resume-from-crash mid-ceremony.
9. **Cross-version protocol gate.** A client whose `protocol_version` falls outside the server's
   range attempts an upload, receives `426`, and the UI surfaces an actionable error.
10. **Model regen after version bump.** Bump the canonical model version; assert stale embeddings
    are excluded from queries; background regen produces fresh embeddings; queries return correct
    results afterwards. Entirely within `capsule-core::ml` and the `capsule-core::db` vector index,
    so it is unaffected by the server rebuild.
11. **Server crash mid-finalization.** Inject a crash between the blob rename and the Postgres
    transaction commit; restart; assert the session moves to `FailedProcessing` cleanly, with no
    orphaned blob and no zombie pending row.
12. **Cross-device enrollment.** Device A authorizes new device B over a verified channel
    (enrollment code plus safety-code check) → B generates hardware keys → A cross-signs B into the
    device directory → B joins each album's MLS group → B's library matches A's. Includes one
    MITM-on-relay abort.
13. **Web drop → adopt.** A browser/WASM client seals a drop to an upload link → the provisioning
    user's native client decapsulates, rewraps the key under the album AMK, and adopts it in place →
    the asset appears in the library and `verify_asset`-accepts on a second device. The only case
    exercising the web/WASM client and the wrapped-key path.
