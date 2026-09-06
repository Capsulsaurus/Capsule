---
title: Module Map
description: Code module to owning design doc to validation tier, and the boundaries between crates
status: draft
---

This map answers one question: for a given piece of code, which design doc owns its contract and
what tier validates it. It says nothing about whether a surface is built — that is the slice
tracker's job, and keeping the answer in one place is what stops this map going stale. To learn
whether something exists today, find its slice: `rg 'S-C16' SLICES.md`.

## Crates and What They Own

| Crate | Ownership |
| --- | --- |
| `capsule-core` | Cryptography (including the MLS album authority), canonical CBOR, validation, CRDTs, sidecars, backup, lifecycle, client filesystem, local SQLite and vector index, import scan/plan/execute, culling, LQIP, share and drop crypto, aggregated federation views, ML orchestration |
| `capsule-server` | The Kynos REST/OpenAPI application — see [Server Modules](#server-modules) |
| `capsule-sdk` | The Spargen-generated REST client plus the orchestration over it Capsule owns: auth and session refresh, the resumable upload state machine, sync, recovery, protocol-version negotiation, LAN peering |
| `capsule-wire` | The response taxonomy shared by server and SDK. Framework-free by construction: `serde` is its only dependency, so neither side's transport choices reach the other |
| `capsule-wasm` | The browser sealing surface `capsule-web` loads — share-link open and guest-drop sealing over `capsule-core` with default features off. Built by `mise run build-wasm`; never committed |
| `capsule-i18n` + `xtask::i18n` | Canonical ICU catalogs, runtime localization, generated platform catalogs |
| `capsule-core-ffi` | UniFFI bindings for native Swift and Kotlin consumers, on one UniFFI version across both surfaces |
| `capsule-core-swift`, `capsule-core-kotlin` | Per-platform harnesses that link the compiled core over those bindings, and the `HardwareSigner` implementations (Secure Enclave, StrongBox) |
| `capsule-cli` + its entity/migration crates | Local CLI behavior and its SQLite persistence; network commands ride the SDK |
| `capsule-web` | The browser client: the guest-drop and share-link viewer surfaces, over `capsule-wasm` |
| `capsule-swift`, `capsule-android` | The native applications |
| `capsule-vision` | Model-evaluation notebooks; no shipped source |

GraphQL and gRPC are retired transports and no compatibility surface will be restored
([ADR-0001](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0001-the-public-server-surface-is-kynos-rest-only.md)).
Review-only sources live under `legacy-review/`: they are not Cargo packages and have no validation
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
| `library::{init,open,rebuild,scrub,cache,lock,paths,receipts,auth_gate}` | [Client Filesystem](/design/filesystem/client/) and [Maintenance](/design/filesystem/maintenance/) | Unit and smoke |
| `library::{space,storage_verify}` | [Import Pipeline](/design/import/pipeline/) and [Storage Verification](/design/import/storage-verification/) | Unit boundary and release-gate tests |
| `import::{scan,scanner,planner,group,special,scope,default_album,importers,streaming,upload,executor,progress}` | [Import Pipeline](/design/import/pipeline/) | Unit; the executor's media half waits on `capsule-core::media` |
| `drop`, `sharing` | [Web Upload](/design/web-upload/) and [Share Links](/design/share-links/) | Unit and KAT; sealing round trips cross-language against `capsule-wasm` |
| `culling` | [Organization — Culling](/design/organization/) | Unit; filtered views and reject sweep |
| `federation` | [Federation](/design/federation/) | Unit; aggregated album view over authority fixtures |
| `ml::{registry,orchestrator,regen,runner}` | [AI](/design/ai/) | Unit determinism; the real inference runner is post-v1 |
| `cohort` | [Authentication](/design/authentication/) | Unit determinism |
| `metadata`, `sidecar` | [Metadata](/design/metadata/) | Unit determinism and round trips |
| `db` | [Client Filesystem](/design/filesystem/client/) | Unit SQLite operations; vector index over `sqlite-vec` |
| `domain`, `models` | [Organization](/design/organization/), [Metadata](/design/metadata/) | Closed-enum and model unit tests |

OpenMLS and the inference engines are implementation dependencies; Capsule retains the application
protocols, schemas, provenance, and policy. Peering is the SDK's, not the core's — it is a transport
between two clients, and the artifact it moves is the backup container.

## Server Modules

`capsule-server` is one Kynos REST/OpenAPI application composed from cohesive internal modules, not
separate public transports or microservices. Route modules under `routes/` carry the HTTP surface;
the modules below carry the behavior behind it.

| Module | Contract owner | Required validation |
| --- | --- | --- |
| `auth`, `enrollment`, `directory` | [Authentication](/design/authentication/) and [Device Enrollment](/design/device-enrollment/) | Unit plus Postgres/Valkey adapter parity |
| `upload` | [Upload Protocol](/design/import/upload-protocol/) | State-machine property tests, adapter parity, smoke and E2E |
| `blob`, `serve` | [Server Filesystem](/design/filesystem/server/) | [Sharded-layout](/design/filesystem/server/#blob-store-layout) round-trip and full-store enumeration, range, corruption, crash, quarantine tests |
| `verify`, `attestation` | [Storage Verification](/design/import/storage-verification/) | Receipt chain continuity, nonce echo, verdict-over-the-same-read tests |
| `gc`, `scrub` | [Filesystem — Maintenance](/design/filesystem/maintenance/) | Refcount mark-and-sweep, retention purge, read-only integrity scrub |
| `sync` | [Download and Sync](/design/import/download-sync/) | Cursor, monotonicity, pagination and range-resume tests |
| `album` | [Authorization](/design/authorization/) and [Versioning](/design/versioning/) | Lifecycle-write authorization, chain advance, upgrade ceremony |
| `share` | [Share Links](/design/share-links/) | Capability and expiry tests |
| `drop`, `escrow` | [Web Upload](/design/web-upload/) and [Backup and Recovery](/design/backup-recovery/) | Drop-session validation, adoption, escrow store/replace |
| `federation` | [Federation](/design/federation/) | Capability, compartmentalization and pull-path tests |
| `quota`, `moderation` | [Quota](/design/quota/) and [Moderation](/design/moderation/) | Unit plus policy smoke tests |
| `store` | [Server Filesystem — Required Services](/design/filesystem/server/#required-services) | One conformance suite every adapter runs, which is what lets the in-memory double be trusted |
| `problem`, `limits`, `body` | [API Surfaces — Rejection Mapping](/design/api-surfaces/#rejection-mapping) | Coded-problem bodies, body-size limits, header census on every route |

The server owns its content-addressed blob implementation behind a Capsule-defined backend trait.
The E2EE-aware resumable protocol also stays in Capsule. Authentication state and upload state use
separate typed ports; no generic CAS, transfer, or TTL library is introduced.

## Client Boundaries

| Boundary | Decision |
| --- | --- |
| REST client | Spargen-generated Rust from a checked-in OpenAPI 3.2 document |
| SDK workflows | Capsule-owned authentication, upload, sync, recovery, and protocol-version orchestration |
| Workspace verbs over FFI | The `capsule_sdk` UniFFI namespace exposes the workspace surface apps need — enroll/open (including a hardware-signer constructor), albums, seal and import, verify, sync-apply, master-key escrow, and device-directory publish. Orchestration and shape only: each verb is one call into `capsule-core`, which keeps every cryptographic step, and the `capsule_core` namespace never shares a binary with it |
| Media | Rawshift performs detection, decode/encode, metadata normalization, derivatives, previews, and video work, consumed through `capsule-core::media` |
| LQIP | Capsule imports Chromahash **0.7.1** directly; Rawshift has no Chromahash responsibility |
| Import commit | Capsule applies privacy policy, creates sidecars/provenance, encrypts, signs, and commits normalized media results |
| Alerts | Alert classes and trigger predicates live in `capsule-core::notify` so every platform evaluates one decision function; scheduling and presentation are native per client. See [Notifications](/design/notifications/) |

## External Dependency Register

These are the intended complexity boundaries. A dependency is not added to an active manifest until
the named acceptance gaps are verified with contract fixtures or a minimal spike.

| Library | Scope Capsule delegates | Acceptance gaps Capsule must verify |
| --- | --- | --- |
| Kynos | HTTP runtime, REST routing, middleware composition, OpenAPI 3.1 and 3.2 emission, limits, shutdown, observability | Streaming request/response bodies, cancellation and backpressure; deterministic schema output; custom protocol/error headers on every response; middleware ordering; test harnesses without live infrastructure |
| Spargen | Rust client generation from the checked-in Kynos OpenAPI contract | OpenAPI 3.1 and 3.2 compatibility; streaming upload/range download; opaque binary bodies; stable error-code mapping; auth and protocol headers; supported Rust targets; deterministic generation and version-compatibility checks |
| Rawshift | Media detection, decoding/encoding, metadata normalization, derivatives, previews, and video processing | Required format/codec matrix; bounded memory and concurrency; cancellation/progress; malformed-input isolation; deterministic orientation/color/HDR behavior; normalized metadata provenance; mobile/desktop targets; no Chromahash API |
| Chromahash **0.7.1** | LQIP encode/decode only, imported directly by Capsule | Deterministic output; wide-gamut/HDR fixtures; decoder fallback behavior; supported FFI targets. The pin and the retired ThumbHash decision are [Dependencies](/design/dependencies/#rust) |
| OpenMLS | MLS protocol and cryptographic state transitions | Required cipher suites and credential model; deterministic persistence/restore; external signer integration; epoch/exporter behavior; cross-platform size/performance; Capsule-owned album policy and provenance stay outside it |
| PostgreSQL driver/ORM (`sea-orm`/`sqlx-postgres`) | The durable server records: the asset index, the account cluster, the device-cohort map, the quota ledger and the library's remaining rows. **Not** the two typed state ports — a Postgres-resident session table is [rejected](/design/filesystem/server/#required-services) as a second implementation of one contract | Transactions needed for finalization, row locking, migration strategy, cancellation, typed error mapping, tracing, and adapter conformance — all discharged for the first four adapters, whose suites run against the deterministic double and against a container under `CAPSULE_TEST_POSTGRES=1`. Migrations are applied by a separate binary and `serve` refuses to boot against a schema it was not built for |
| `redis-rs` | Required Valkey adapters for `AuthStateStore`, `UploadSessionStore` and the ceremony stores — the volatile half, and the only production adapter any of them gets | Atomic compare/update and expiry primitives required by each port; cluster behavior; cancellation/timeouts; tracing; behavioural parity with the in-memory double under one conformance suite — parity is what lets that double be trusted in tests, not a claim that Valkey is [substitutable](/design/filesystem/server/#required-services). Parity with the PostgreSQL adapters is not a goal, because no port has both |
| RustCrypto, `ciborium`, `rusqlite`, `sqlite-vec`, UniFFI, `wasm-bindgen` | Existing crypto primitives, canonical serialization, local catalog and vector index, native bindings, and the browser boundary | Continue vectors, canonical-byte tests, migration tests, and binding smoke tests; these libraries do not own Capsule protocols or schemas |

Explicit non-dependencies: no generic CAS crate, `object_store`, resumable-transfer library, generic
TTL/CAS library, GraphQL/gRPC stack, or in-repository media codec stack. Reconsider extraction only
after a product-neutral interface has two real consumers and removes more audit surface than it adds.

## E2E Test Surface

The E2E surface is **bounded**: adding a test here means adding it to the relevant doc's Validation
section and justifying why the cross-module surface is irreducible. Each case must remain backed
primarily by unit and adapter-contract tests. Cases are numbered so that code can name the case it
covers (`rg "E2E case N"`), and slices in the repo-root `SLICES.md` reference these numbers.

1. **Auth → sync → client-side library query.** Sign in → access token → the sync feed returns the
   account's album entries → the client applies them and a local `library.sqlite` query lists the
   expected albums (rich queries are client-side per [API Surfaces](/design/api-surfaces/)).
2. **Full import + upload + finalize.** Local scan → plan → execute → upload session → finalize →
   blob present at its [content address](/design/filesystem/server/#blob-store-layout) and the index
   row marked uploaded.
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
    transaction commit; assert the session moves to `FailedProcessing` cleanly, the asset row is
    still `Pending` with no sequence number, and **nothing references the blob** — no dangling
    reference, which is the one outcome the finalization order exists to forbid
    ([Filesystem — Server](/design/filesystem/server/)). An **orphaned blob is permitted** and is
    the safe half of that trade: the bytes are at their content address with a reference count of
    zero, the collector marks them, and the client's retry re-references them because
    `BlobStore::commit` is idempotent on identical ciphertext. This row previously asked for "no
    orphaned blob", which contradicted the design it cites and the test that asserts it.
    The crash is injected through the `AssetIndex` port, so no production code carries a test
    hook; the **process-restart** variant — a real kill, and a second process over the same blob
    root and database — is owed to the remaining durable adapters and belongs to the binary-smoke
    tier.
12. **Cross-device enrollment.** Device A authorizes new device B over a verified channel
    (enrollment code plus safety-code check) → B generates hardware keys → A cross-signs B into the
    device directory → B joins each album's MLS group → B's library matches A's. Includes one
    MITM-on-relay abort.
13. **Web drop → adopt.** A browser/WASM client seals a drop to an upload link → the provisioning
    user's native client decapsulates, rewraps the key under the album AMK, and adopts it in place →
    the asset appears in the library and `verify_asset`-accepts on a second device. The only case
    exercising the web/WASM client and the wrapped-key path.
