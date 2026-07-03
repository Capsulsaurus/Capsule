---
title: Module Map
description: Index of every code module to its owning design doc and validation tier
---

This is the developer's first stop. It maps every Capsule workspace crate and module to the design doc(s) that govern its behavior, and to the validation tier (Unit / Smoke / E2E — see [Validation Tiers](/design/principles/#validation-tiers)) it ships with. The E2E test surface at the bottom is **bounded**: adding a test there means adding the test to the relevant doc's Validation section and justifying why the cross-module surface is irreducible.

The mapping reflects the *design intent*. Modules not yet implemented are annotated `(planned)` — or `(blocked)` where a named upstream dependency gates them — per the [status convention](/design/principles/#implementation-status); every unannotated module exists in the codebase today. The repo-root `SLICES.md` indexes the planned modules as executable implementation slices.

## Crate Roster

| Crate                     | Purpose                                                                                                           |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `capsule-core`            | Shared logic across server and clients: cryptography, library layout, import pipeline, metadata, ML orchestration |
| `capsule-sdk`             | Client SDK: auto-generated OpenAPI client, upload protocol, LAN-peering glue                                      |
| `capsule-core-ffi`        | uniffi bindings crate for native Swift/Kotlin consumers (sidecar `Catalog` surface; the `HardwareSigner` foreign trait ships under `capsule-core`'s `ffi` feature) |
| `capsule-api`             | Server entry-point + routing                                                                                      |
| `capsule-api-auth`        | Authentication, sessions, OIDC, device directory                                                                  |
| `capsule-api-library`     | **Legacy** GraphQL API for UI queries — frozen and retiring; rich queries move client-side onto `library.sqlite` (see [API Surfaces](/design/api-surfaces/#legacy-graphql-retiring)) |
| `capsule-api-upload`      | TUS-like resumable upload protocol server                                                                         |
| `capsule-api-media`       | Media serving (ciphertext blobs, public shares)                                                                   |
| `capsule-api-sync`        | gRPC sync API + federation                                                                                        |
| `capsule-api-service`     | Higher-level service layer over the entity model (album, asset, friendship, passkey, stack, user, quota)          |
| `capsule-api-entity`      | Sea-ORM entities (Postgres schema)                                                                                |
| `capsule-api-model`       | Business-logic models on top of entities                                                                          |
| `capsule-api-migration`   | Sea-ORM migrations                                                                                                |
| `capsule-api-environment` | Configuration, env vars, feature flags                                                                            |
| `capsule-api-testing`     | Shared test utilities (testcontainer setup, schema fixtures)                                                      |
| `capsule-cli`             | Command-line client                                                                                               |
| `capsule-media`           | Standalone media utility crate                                                                                    |
| `capsule-i18n`            | Runtime localization (locale negotiation + ICU message formatting) for the server and CLI                         |
| `capsule-web`             | Browser/WASM web-upload client: guest drop sealing + upload (planned)                                             |
| `capsule-vision`          | Python research package for on-device ML experimentation (not a cargo crate)                                      |

Native app and harness packages — `capsule-android`, `capsule-swift`, `capsule-core-swift`, `capsule-core-kotlin` — are not cargo crates and carry no design owner of their own; the two harness packages hold the per-platform `HardwareSigner` reference adapters ([Keys — Device Keys](/design/cryptography/keys/#device-keys)).

## Module → Design Doc

### `capsule-core`

| Module                                                                  | Owning design doc                                                                                              | Validation tier                               |
| ----------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| `crypto::primitives`                                                    | [Cryptography — Primitives](/design/cryptography/primitives/)                                                  | Unit (RFC vectors)                            |
| `crypto::keys`                                                          | [Cryptography — Keys](/design/cryptography/keys/), [Device Enrollment](/design/device-enrollment/)             | Unit + Smoke (hardware per-platform)          |
| `crypto::authority`                                                     | [Cryptography — Keys: Write Authority Interface](/design/cryptography/keys/#write-authority-interface)         | Unit (epoch-ledger round-trip)                |
| `crypto::mls` (blocked upstream — see the [MLS status note](/design/cryptography/mls/)) | [Cryptography — MLS](/design/cryptography/mls/), [MLS Resilience](/design/mls-resilience/)     | Unit + Smoke (protocol round-trip)            |
| `crypto::encryption`                                                    | [Cryptography — Encryption](/design/cryptography/encryption/)                                                  | Unit (KAT, round-trip)                        |
| `crypto::provenance`                                                    | [Cryptography — Provenance](/design/cryptography/provenance/)                                                  | Unit (exhaustive negative cases) + Smoke      |
| `crypto::verify_asset`                                                  | [Cryptography — Write Authorization](/design/cryptography/keys/#write-authorization)                           | Unit (the single chokepoint; exhaustive)      |
| `validation`                                                            | [Threat Model — Validation](/design/threat-model/validation/)                                                  | Unit (pure key-free invariants)               |
| `cbor`                                                                  | [Metadata — Canonical CBOR](/design/metadata/#canonical-cbor-encoding)                                         | Unit (RFC 8949 §4.2 vectors)                  |
| `lifecycle`                                                             | [Filesystem — Client](/design/filesystem/client/), [Import — Pipeline](/design/import/pipeline/)               | Unit + Smoke (`capsule demo` end-to-end)      |
| `backup`                                                                | [Backup and Recovery](/design/backup-recovery/)                                                                | Unit + Smoke                                  |
| `library::{init,open,rebuild,lock,paths,scrub,trash}`                   | [Filesystem — Client](/design/filesystem/client/), [Filesystem — Maintenance](/design/filesystem/maintenance/) | Unit + Smoke                                  |
| `import::{scanner,planner,executor,plan,upload,group,progress,special}` (streaming upload loop planned) | [Import — Pipeline](/design/import/pipeline/)                                                  | Unit (planner determinism) + Smoke (executor) |
| `metadata::{file,filter,types}`                                         | [Metadata](/design/metadata/)                                                                                  | Unit (filtering)                              |
| `sidecar::*`                                                            | [Metadata — Sidecar Schema](/design/metadata/#sidecar-schema-v1)                                               | Unit (serde determinism)                      |
| `exif::{extract,timezone}`                                              | [Metadata](/design/metadata/)                                                                                  | Unit                                          |
| `db::{driver,schema,rows}`                                              | [Filesystem — Client](/design/filesystem/client/)                                                              | Unit (SQLite ops)                             |
| `domain::*` (enums)                                                     | [Organization](/design/organization/), [Authorization](/design/authorization/), [Metadata](/design/metadata/)  | Unit (closed-enum rejection)                  |
| `models::*`                                                             | [Metadata](/design/metadata/), [Import — Pipeline](/design/import/pipeline/)                                   | Unit                                          |
| `ml` (planned)                                                          | [AI/ML Integrations](/design/ai/)                                                                             | Unit + Smoke (inference parity per-platform)  |
| `sharing` (planned)                                                     | [Share Links](/design/share-links/)                                                                            | Unit                                          |
| `drop` (planned)                                                        | [Web Upload](/design/web-upload/)                                                                              | Unit (seal round-trip + adoption rewrap)      |

### `capsule-sdk`

| Module                    | Owning design doc                                                                                  | Validation tier                       |
| ------------------------- | -------------------------------------------------------------------------------------------------- | ------------------------------------- |
| (auto-generated client)   | [Clients](/design/clients/)                                                                        | Smoke (re-generated; not unit-tested) |
| `upload` (client stub)    | [Import — Upload Protocol](/design/import/upload-protocol/)                                        | Unit + Smoke (client side)            |
| `peering` (planned)       | [Peering](/design/peering/)                                                                        | Unit + Smoke per platform             |

Hardware-key adapters do **not** live in `capsule-sdk`: the `Signer`/`HardwareSigner` seams and the software + TPM reference adapters are in `capsule-core::crypto::keys`, and the Secure Enclave / StrongBox adapters live in the `capsule-core-swift` / `capsule-core-kotlin` harness packages (see [Keys — Device Keys](/design/cryptography/keys/#device-keys)).

### `capsule-api` (root + sub-crates)

| Module                                               | Owning design doc                                                                    | Validation tier                             |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------- |
| `capsule-api` (routing)                              | [Filesystem — Server](/design/filesystem/server/)                                    | Smoke                                       |
| `capsule-api-auth::{oidc,session,claims,roles}`      | [Authentication](/design/authentication/), [Authorization](/design/authorization/)   | Unit + Smoke (testcontainer Postgres/Valkey) |
| `capsule-api-auth::devices` (planned for enrollment) | [Device Enrollment](/design/device-enrollment/)                                      | Smoke                                       |
| `capsule-api-library::schema::*` (legacy, retiring)  | [API Surfaces](/design/api-surfaces/#legacy-graphql-retiring)                        | frozen (no new surface)                     |
| `capsule-api-library::loaders` (legacy, retiring)    | [API Surfaces](/design/api-surfaces/#legacy-graphql-retiring)                        | frozen (no new surface)                     |
| `capsule-api-upload`                                 | [Import — Upload Protocol](/design/import/upload-protocol/)                          | Unit + Smoke + 1 E2E                        |
| `capsule-api-media::routes`                          | [Filesystem — Server](/design/filesystem/server/), [Thumbnails](/design/thumbnails/) | Smoke                                       |
| `capsule-api-media::verify` (planned)                | [Import — Storage Verification](/design/import/storage-verification/) | Unit + Smoke                                |
| `capsule-api-media::shares` (planned)                | [Share Links](/design/share-links/)                                                  | Unit + Smoke                                |
| `capsule-api-media::drops` (planned)                 | [Web Upload](/design/web-upload/)                                                    | Unit + Smoke                                |
| `capsule-api-sync` (sync feed)                       | [Import — Download & Sync](/design/import/download-sync/)                            | Unit + Smoke + 1 E2E                        |
| `capsule-api-sync::federation`                       | [Federation](/design/federation/)                                                    | Unit + Smoke + 1 E2E                        |
| `capsule-api-service::album`                         | [Organization](/design/organization/)                                                | Unit                                        |
| `capsule-api-service::asset`                         | [Authorization](/design/authorization/), [Organization](/design/organization/)       | Unit + Smoke                                |
| `capsule-api-service::quota` (planned)               | [Quota](/design/quota/)                                                              | Unit                                        |
| `capsule-api::moderation` (planned)                  | [Moderation](/design/moderation/)                                                    | Smoke                                       |
| `capsule-api-entity::*` (Sea-ORM)                    | [Filesystem — Server](/design/filesystem/server/)                                    | Unit (Sea-ORM CRUD)                         |
| `capsule-api-migration`                              | [Versioning](/design/versioning/) (forward-only migrations)                          | Smoke (migration run)                       |
| `capsule-api-environment`                            | (configuration; no design owner)                                                     | Unit                                        |
| `capsule-api-testing`                                | (test utilities; no design owner)                                                    | n/a                                         |

### `capsule-cli`, `capsule-media`, `capsule-i18n`, `capsule-web`

| Crate           | Owning design doc                                                 | Validation tier      |
| --------------- | ----------------------------------------------------------------- | -------------------- |
| `capsule-cli`   | [Clients](/design/clients/) (treats CLI as a client)              | Smoke                |
| `capsule-media` | [Thumbnails and Previews](/design/thumbnails/) (decode/encode utilities; only JPEG decode is implemented today) | Unit                 |
| `capsule-i18n`  | [Internationalization](/design/i18n/)                             | Unit + Smoke         |
| `capsule-web`   | [Web Upload](/design/web-upload/), [Clients](/design/clients/)    | Smoke (browser/WASM) |

## Design Doc → Module (Reverse Lookup)

Navigation from a design doc back to where the code lives.

| Design doc                                                          | Implementing modules                                                                                                          |
| ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| [Principles](/design/principles/)                                   | (meta — no specific code module)                                                                                              |
| [Cryptography — Primitives](/design/cryptography/primitives/)       | `capsule-core::crypto::primitives`                                                                                            |
| [Cryptography — Keys](/design/cryptography/keys/)                   | `capsule-core::crypto::keys` (`Signer`/`HardwareSigner` seams; software + TPM reference adapters; Secure Enclave / StrongBox adapters in `capsule-core-swift`/`-kotlin`; P-256 hybrid-DSK variant planned) |
| [Cryptography — Keys: Write Authority Interface](/design/cryptography/keys/#write-authority-interface) | `capsule-core::crypto::authority` (`AlbumAuthority` trait + `ReferenceAuthority` epoch ledger; `OpenMlsAuthority` blocked with MLS) |
| [Cryptography — MLS](/design/cryptography/mls/)                     | `capsule-core::crypto::mls` (blocked upstream — no usable OpenMLS backend for the `0x004D` ciphersuite; see the [MLS status note](/design/cryptography/mls/)) |
| [Cryptography — Encryption](/design/cryptography/encryption/)       | `capsule-core::crypto::encryption`                                                                                            |
| [Cryptography — Provenance](/design/cryptography/provenance/)       | `capsule-core::crypto::provenance` + `verify_asset` chokepoint                                                                |
| [Cryptography — Failure Modes](/design/cryptography/failure-modes/) | Cross-cutting: `capsule-core::backup`, `capsule-core::library`, `capsule-core::crypto::*`                                     |
| [MLS Resilience](/design/mls-resilience/)                           | `capsule-core::crypto::mls` (extends main MLS module; blocked with it)                                                        |
| [Device Enrollment](/design/device-enrollment/)                     | `capsule-core::crypto::keys`, `capsule-api-auth::devices`                                                                     |
| [Authentication](/design/authentication/)                           | `capsule-api-auth::{oidc,session,claims}`                                                                                     |
| [Authorization](/design/authorization/)                             | `capsule-api-auth::roles`, `capsule-core::crypto::provenance` (verify_asset)                                                  |
| [Clients](/design/clients/)                                         | `capsule-sdk` + per-platform native code                                                                                      |
| [Internationalization](/design/i18n/)                               | `capsule-i18n` (runtime) + `xtask::i18n` (codegen) + `locales/` source + per-platform generated catalogs                     |
| [Versioning](/design/versioning/)                                   | Cross-cutting: `capsule-api` (header enforcement), `capsule-core::crypto::mls` (upgrade ceremony), `capsule-api-migration`    |
| [Backup and Recovery](/design/backup-recovery/)                     | `capsule-core::backup`, `capsule-api-auth` (escrow surface, planned)                                                          |
| [Metadata](/design/metadata/)                                       | `capsule-core::{metadata,sidecar,exif}`, `capsule-api-library::schema`                                                        |
| [Filesystem — Server](/design/filesystem/server/)                   | `capsule-api`, `capsule-api-entity`, blob store glue                                                                          |
| [Filesystem — Client](/design/filesystem/client/)                   | `capsule-core::{library,db}`, per-platform native code                                                                        |
| [Filesystem — Maintenance](/design/filesystem/maintenance/)         | `capsule-core::library::{scrub,rebuild,trash}`, server-side scrub in `capsule-api-upload`                                     |
| [Import — Pipeline](/design/import/pipeline/)                       | `capsule-core::import::*`; the streaming import-upload loop and the `capsule-core::library::available_bytes` free-space probe are planned |
| [Import — Upload Protocol](/design/import/upload-protocol/)         | `capsule-sdk::upload` (client) + `capsule-api-upload` (server)                                                                |
| [Import — Download & Sync](/design/import/download-sync/)           | `capsule-sdk` (client) + `capsule-api-sync` (server)                                                                          |
| [Import — Storage Verification](/design/import/storage-verification/) | `capsule-api-media::verify` (route) + `capsule-sdk` (client) + `capsule-core` (verify-before-destroy predicate)              |
| [Federation](/design/federation/)                                   | `capsule-api-sync::federation`                                                                                                |
| [Peering](/design/peering/)                                         | `capsule-sdk::peering` (planned) + `capsule-core::backup` (artifact format)                                                   |
| [Organization](/design/organization/)                               | `capsule-core::domain::stack_type`, `capsule-api-service::{album,stack}`                                                      |
| [AI/ML Integrations](/design/ai/)                                   | `capsule-core::ml` (planned), `capsule-vision` (Python research), model registry + per-platform inference runners             |
| [Thumbnails](/design/thumbnails/)                                   | Client-side gen in `capsule-sdk` (planned) over `capsule-media` decode/encode utilities + serving in `capsule-api-media`      |
| [Share Links](/design/share-links/)                                 | `capsule-core::sharing` (planned), `capsule-api-media::shares` (planned)                                                      |
| [Web Upload](/design/web-upload/)                                   | `capsule-core::drop` (planned), `capsule-api-media::drops` (planned), `capsule-web` (planned); reuses `capsule-api-upload` for drop chunks |
| [Moderation](/design/moderation/)                                   | `capsule-api::moderation` (planned)                                                                                           |
| [Quota](/design/quota/)                                             | `capsule-api-service::quota` (planned)                                                                                        |
| [Threat Model](/design/threat-model/)                               | Enforced across every validation chokepoint: `capsule-core::crypto::verify_asset` (client), `capsule-api` validators (server) |
| [Threat Model — Scenarios](/design/threat-model/scenarios/)         | (catalog; each row maps to the owner doc's module)                                                                            |
| [Threat Model — Validation](/design/threat-model/validation/)       | `capsule-api` envelope checks (server-side), `capsule-core::crypto::verify_asset` (client-side)                               |
| [Threat Model — Schema Rules](/design/threat-model/schema-rules/)   | `capsule-core::crypto` decoders + `capsule-api` validators (closed-enum + Postel asymmetry)                                   |

## E2E Test Surface

The bounded global list of cross-module integration tests. Editing this list requires updating the relevant doc's Validation section. **Adding an E2E case past this list is a signal the design has unwanted coupling worth examining** before adding the test.

Target count: ≤ 13 cases. Each one is named by what it proves — not "test X" but "X works through Y and Z."

**Status:** none of the 13 cases is runnable today — every one crosses at least one planned surface (the networked server/client wiring, MLS, or ML), even where its `capsule-core` half is implemented and unit/smoke-tested. The bound starts governing with the first implementation slice that makes a case live; until then this list is the contract the slices build toward.

1. **Auth → sync → client-side library query.** Log in via OIDC → access token → gRPC `Sync` returns the account's album entries → the client applies them and a local `library.sqlite` query lists the expected albums (rich queries are client-side per [API Surfaces](/design/api-surfaces/)). Covers `capsule-api-auth::oidc + session` × `capsule-api-sync` × `capsule-core::library`.
2. **Full import + upload + finalize.** Local scan → plan → execute → upload session → finalize → blob present at `blobs/{hash}` + index row marked uploaded. Covers `capsule-core::import` × `capsule-sdk::upload` × `capsule-api-upload` × `capsule-api-entity`.
3. **Sync feed pickup.** Upload from device A → device B's sync feed advances → device B fetches metadata blob and (per scope) the original. Covers `capsule-api-sync` × `capsule-sdk` download path × `capsule-core::library` write.
4. **Federation cross-server pull.** Alice on `home.tld` shares to Bob on `other.tld` → capability token → Bob's server pulls metadata + blobs → Bob's client renders. Covers `capsule-api-sync::federation` (both sides) × `capsule-api-auth` (capability issue).
5. **LAN peering A→B.** Two devices on the same LAN; mDNS discovery → TLS handshake → delta-scoped artifact → restore on receiver → byte-equal libraries. Covers `capsule-sdk::peering` × `capsule-core::backup` × `capsule-core::library`.
6. **Backup → restore on a fresh device.** Export full backup → bootstrap new device via passphrase + escrow → import backup → assert every asset present and verifiable. Covers `capsule-core::backup` × `capsule-core::crypto::keys` × `capsule-core::library`.
7. **Full lifecycle.** Create → metadata-update → trash → restore → re-delete → hard-purge after retention. Provenance chain advances through every transition; server refuses purge before `retention_until`. Covers `capsule-api-auth::roles` × `capsule-core::crypto::provenance` × server purge worker.
8. **Album upgrade ceremony.** Multi-member album; admin initiates upgrade → quiesce → drain → tombstone → fork → queued writes replay. Includes one resume-from-crash mid-ceremony. Covers `capsule-core::crypto::mls` × `capsule-api` × client UI.
9. **Cross-version protocol gate.** Client with `protocol_version` outside server's range attempts upload; receives `426`; UI surfaces actionable error. Covers `capsule-api` handshake × `capsule-sdk` error handling.
10. **Model regen after version bump.** Bump canonical model version; assert stale embeddings excluded from queries; background regen produces fresh embeddings; queries return correct results post-regen. Covers `capsule-core::ml` × `capsule-core::db` vector index.
11. **Server crash mid-finalization.** Inject crash between blob rename and Postgres transaction commit; restart; assert session moves to `FailedProcessing` cleanly, no orphaned blob, no zombie pending row. Covers `capsule-api-upload` × `capsule-api-entity` × `capsule-api`'s startup scrub.
12. **Cross-device enrollment.** Existing device A authorizes new device B over a verified channel (enrollment code + safety-code check) → B generates hardware keys → A cross-signs B into the device directory → B joins each album's MLS group → B's library matches A's. Includes one MITM-on-relay abort. Covers `capsule-api-auth::devices` × `capsule-core::crypto::keys` × `capsule-sdk::hardware-keys`.
13. **Web drop → adopt.** A browser/WASM web client seals a drop to an upload link → the provisioning user's native client decapsulates, rewraps the key under the album AMK, and adopts it in place → the asset appears in the library and `verify_asset`-accepts on a second device. The only case exercising the web/WASM client and the wrapped-key path. Covers `capsule-web` × `capsule-api-media::drops` × `capsule-core::drop` × `capsule-api-upload` (drop chunks) × `capsule-core::crypto` (rewrap + `verify_asset`) × `capsule-core::library`.

## Using This Map

- **When implementing a module:** find it in [Module → Design Doc](#module--design-doc), open the owning doc, read the contracts and the validation tier expectations. The unit + smoke surface defined in that doc should be authorable without leaving the module.
- **When adding a feature:** find the relevant design doc via the [reverse lookup](#design-doc--module-reverse-lookup); confirm the feature fits within an existing module's scope or warrants a new one. If new, add a row here.
- **When considering an E2E test:** check this list first. If your proposed test isn't here, either it's an existing case in disguise (use that), or the design has cross-module coupling worth surfacing — discuss before adding.
