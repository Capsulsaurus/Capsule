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
| `capsule-core` | Active | Cryptography, canonical CBOR, validation, CRDTs, sidecars, backup, lifecycle, client filesystem, local SQLite, import scan/plan, share/drop contract skeletons |
| `capsule-i18n` + `xtask::i18n` | Active | Canonical ICU catalogs, runtime localization, generated platform catalogs |
| `capsule-core-ffi` | Active, consolidation pending | Legacy low-level UniFFI surface; the high-level `capsule-core` FFI is the intended destination |
| `capsule-cli` + CLI entity/migration crates | Active, consolidation pending | Local CLI behavior and its legacy SQLite persistence stack |
| Client import execution | Quarantined | Rebuild over Rawshift plus direct Chromahash v1; scan, grouping, and planning remain active |
| Server | Quarantined | Rebuild with Kynos as REST/OpenAPI-only `capsule-api` modules |
| Rust SDK | Quarantined | Regenerate with Spargen from the canonical Kynos OpenAPI document |
| Media pipeline | Quarantined | Rawshift replaces in-repository codecs and metadata extraction |
| GraphQL and gRPC transports | Deleted | Rejected; no compatibility surface will be restored |

Review-only sources live under `legacy-review/`. They are not Cargo packages and have no validation
status until rewritten against their owning contracts.

## Active `capsule-core` Ownership

| Module | Owning design | Validation |
| --- | --- | --- |
| `crypto::{primitives,keys,encryption,provenance,verify_asset}` | [Cryptography](/design/cryptography/) and [Authorization](/design/authorization/) | Unit vectors, negative cases, smoke |
| `backup` | [Backup and Recovery](/design/backup-recovery/) | Unit and smoke |
| `library::{init,open,rebuild,scrub,trash,cache}` | [Client Filesystem](/design/filesystem/client/) and [Maintenance](/design/filesystem/maintenance/) | Unit and smoke |
| `import::{scanner,planner,group,special}` | [Import Pipeline](/design/import/pipeline/) | Unit; executor smoke is blocked on Rawshift |
| `drop`, `sharing` | [Web Upload](/design/web-upload/) and [Share Links](/design/share-links/) | Contract tests; crypto behavior remains sliced |
| `cohort` | [Authentication](/design/authentication/) | Unit determinism |
| `library::{space,storage_verify}` | [Import Pipeline](/design/import/pipeline/) and [Storage Verification](/design/import/storage-verification/) | Unit boundary and release-gate tests |
| `metadata`, `sidecar` | [Metadata](/design/metadata/) | Unit determinism and round trips |
| `db` | [Client Filesystem](/design/filesystem/client/) | Unit SQLite operations |
| `domain`, `models` | [Organization](/design/organization/), [Metadata](/design/metadata/) | Closed-enum and model unit tests |

MLS, sharing, peering, and ML remain planned. OpenMLS and inference engines are implementation
dependencies; Capsule retains the application protocols, schemas, provenance, and policy.

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
| RustCrypto, `ciborium`, `rusqlite`, UniFFI | Existing crypto primitives, canonical serialization, local catalog, and native bindings | Continue vectors, canonical-byte tests, migration tests, and binding smoke tests; these libraries do not own Capsule protocols or schemas |

Explicit non-dependencies: no generic CAS crate, `object_store`, resumable-transfer library, generic
TTL/CAS library, GraphQL/gRPC stack, or in-repository media codec stack. Reconsider extraction only
after a product-neutral interface has two real consumers and removes more audit surface than it adds.

## E2E Test Surface

The future irreducible E2E cases are authentication-to-authorized REST, import-to-upload-to-storage
verification, sync feed pickup, federation pull, backup restore, hardware-signer import, and server
crash during finalization. Each must remain backed primarily by unit and adapter-contract tests.
