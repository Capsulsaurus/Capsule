---
title: Cryptographic Primitives
description: Single-source-of-truth inventory of every cryptographic primitive Capsule uses
---

This doc is **the single source of truth** for every cryptographic primitive Capsule uses. Other docs (and the rest of the cryptography sub-docs) reference these by anchor — they never restate the choice. Swapping a primitive is a single-row edit here, plus a new `crypto_suite_id` and the dedicated section below.

The primitive identities themselves live in `capsule-core::crypto::primitives` as compile-time constants and tagged enums. Every wire format and on-disk record that depends on a primitive carries the [versioning identifiers](#versioning-identifiers) below, so two structures encrypted under different suite versions can coexist without a flag day.

## Primitives Inventory

| Primitive                                                           | Choice                                                                          | Used for                                               |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------ |
| [Cryptographic hash](#cryptographic-hash)                           | SHA-256                                                                         | Content addressing, integrity verification             |
| [Key derivation (KDF)](#key-derivation)                             | HKDF-SHA512                                                                     | Per-file and per-album key derivation                  |
| [Password-based KDF](#password-based-kdf)                           | Argon2id (device-tier-aware parameters)                                         | Master-key escrow unwrap, backup unwrap                |
| [Bulk AEAD](#bulk-aead)                                             | AES-256-GCM with [STREAM](/design/cryptography/encryption/#stream-construction) | Asset and metadata ciphertext                          |
| [MLS control AEAD](#mls-control-aead)                               | ChaCha20-Poly1305                                                               | Inherited from the [MLS ciphersuite](#mls-ciphersuite) |
| [Signature scheme](#signature-scheme)                               | Hybrid Ed25519 + ML-DSA-65                                                      | Identity, device, asset manifest, write tier           |
| [KEM](#kem)                                                         | X-Wing (X25519 + ML-KEM-768)                                                    | MLS HPKE                                               |
| [MLS ciphersuite](#mls-ciphersuite)                                 | `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (0x004D)                        | Group key management                                   |
| [Randomness](#randomness)                                           | OS CSPRNG (`getrandom`)                                                         | All keys, salts, nonces                                |
| [Transport](/design/cryptography/failure-modes/#transport-security) | TLS 1.3 with hybrid X25519+ML-KEM                                               | Client-server, server-server                           |

The per-primitive sections below carry the rationale; the table is the at-a-glance reference.

## Versioning Identifiers

A faulty, malicious, or version-mismatched client could damage data by writing under a primitive set the receiving side does not implement (see [Threat Model](/design/threat-model/)). Three identifiers — owned here, in [Versioning](/design/versioning/), and in [Metadata](/design/metadata/) — bind each on-disk and on-wire structure to a specific set of primitives or schema so mismatches **fail closed** rather than corrupt state:

| Identifier         | Type                | Declared in                                                      | Carried in                                                                                                                                                                                                      |
| ------------------ | ------------------- | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crypto_suite_id`  | `u16`               | this doc                                                         | every [AssetManifest](/design/cryptography/provenance/#asset-manifest), every [metadata blob](/design/cryptography/encryption/#metadata-blob-wire-format), the backup [MANIFEST.cbor](/design/backup-recovery/) |
| `protocol_version` | string `YYYY-MM-DD` | [Versioning](/design/versioning/)                                | every AssetManifest, every wire request (see [Threat Model — Protocol Negotiation](/design/threat-model/validation/#protocol-and-capability-negotiation)), the album's MLS pin                                  |
| `sidecar_schema`   | `u16`               | [Metadata — Sidecar Schema](/design/metadata/#sidecar-schema-v1) | CBOR sidecar field 0 (readable before parsing the rest)                                                                                                                                                         |

`crypto_suite_id = 0x0001` denotes exactly the [Primitives Inventory](#primitives-inventory) above. Retiring any primitive (a broken SHA-256, a deprecated AEAD) **does not edit the row** — it adds a new row and a new suite id. An old AssetManifest carrying `0x0001` keeps verifying against the original row forever; new writes use the new suite id. This is the single-doc edit the inventory promises, generalized to the bundle.

The signatures on every manifest cover `crypto_suite_id` and `protocol_version`, so a downgrade-attempt (re-signing an existing manifest under a weaker suite) cannot be silently produced.

### Backward Compatibility

Old suite ids and protocol versions remain decryptable forever: every encryption-metadata structure is versioned in-band, with its parameters (e.g. Argon2id memory/iterations) saved inside the construction, so a future change never breaks a previous construction. Clients outside the server's supported `protocol_version` range are rejected at the [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation), before any state is written.

## Per-Primitive Choices

### Cryptographic Hash

**SHA-256** (SHA-2) for all content hashing, addressing, and integrity verification — one hash algorithm everywhere: the most prevalent, audited, NIST-approved standard, hardware-accelerated on every target, and one fewer implementation to maintain.

The same SHA-256 value is reused across layers rather than recomputed — the content-addressing hash (see [Asset Encryption](/design/cryptography/encryption/#authenticated-asset-encryption)) is the value the [signed manifest](/design/cryptography/provenance/#asset-manifest) commits to and the upload protocol declares and verifies. Rejected: SHA-3 (weaker hardware support); BLAKE3 (parallelism unneeded given concurrent uploads, keyed mode redundant against our already-authenticated encryption).

### Key Derivation

**HKDF-SHA512** for per-file and per-album key derivation. The wider hash keeps the stack's PQ posture: under Grover a 256-bit hash falls to ~128-bit security while SHA-512 retains ~256-bit, and KDFs are off the hot path so the cost is negligible.

Every derivation includes a versioned `info` string (e.g. `"asset-file/v1"`) and a scope-unique salt (`album_id`, `file_id`), so a future KDF change lands alongside v1 derivations without a flag day.

### Password-based KDF

**Argon2id** with device-tier-aware parameters (canonical defaults below). It runs only at account recovery and device bootstrap — never on a hot path — so the cost is acceptable even on constrained hardware. Each tier's parameters are recorded inside the wrapped blob, so they can be raised as device telemetry accrues without a flag day.

| Device tier             | Memory  | Iterations (`t`) | Parallelism (`p`) | When applies                             |
| ----------------------- | ------- | ---------------- | ----------------- | ---------------------------------------- |
| Low-RAM (≤ 2 GiB total) | 128 MiB | 3                | 1                 | Entry-level Android, low-end embedded    |
| Normal mobile / laptop  | 256 MiB | 3                | 1                 | Default for phones and laptops           |
| Desktop (≥ 8 GiB)       | 512 MiB | 4                | 1                 | Wrapping new escrow blobs from a desktop |

The salt is always a 32-byte CSPRNG draw. The tier chosen at *wrap* time is recorded in the blob; *unwrap* respects whatever tier was recorded, so a desktop-wrapped blob unwraps correctly on a phone (slowly) and vice versa.

### Bulk AEAD

**AES-256-GCM**. Combined with the [STREAM construction](/design/cryptography/encryption/#stream-construction) it covers asset ciphertext; standalone AES-256-GCM (fresh random nonce per blob) covers CBOR metadata blobs.

- AES hardware acceleration (Intel AES-NI, ARMv8 AES extensions, Apple Silicon dedicated AES units) is universal on every platform Capsule targets, so AEAD is never the bottleneck.
- AES-GCM over ChaCha20-Poly1305 for stack consistency with the [SHA-2 family](#cryptographic-hash) and to keep one bulk-AEAD choice across the codebase. MLS retains ChaCha20-Poly1305 from its [ciphersuite spec](#mls-ciphersuite); that's a separate layer.
- Nonce misuse is the structural risk of GCM. Closed two ways: every file uses a freshly-derived per-file key (so the STREAM counter can safely start at zero), and standalone metadata blobs each draw a fresh CSPRNG nonce.

### MLS Control AEAD

**ChaCha20-Poly1305**, inherited from the [MLS ciphersuite](#mls-ciphersuite). This protects MLS's own membership and key messages, not user data; user data uses the [bulk AEAD](#bulk-aead) above.

### Signature Scheme

**Hybrid Ed25519 + ML-DSA-65** for all long-lived **identity** signatures: the user IK, device keys, asset manifests, and write-tier keys. Both halves must verify before a peer is accepted, so neither algorithm being broken alone compromises authentication.

**Short-lived operational signatures are classical Ed25519 only** — server-to-server federation, [federation capability tokens](/design/federation/#federation-capabilities), and [access-token JWTs](/design/authentication/#access-token). These live minutes to hours and rotate cheaply, so PQ hybridization buys no meaningful margin (a harvest-now-decrypt-later adversary gains nothing from a long-expired signature) and is not worth the wire-size and verification cost. This carve-out is owned here; consumers link to it rather than restating the choice.

MLS LeafNode signatures stay Ed25519-only (pinned by the ciphersuite); the ML-DSA half of a device's identity lives at the identity layer — see [MLS](/design/cryptography/mls/).

### KEM

**X-Wing (X25519 + ML-KEM-768)**. This is the KEM defined by the [MLS ciphersuite](#mls-ciphersuite) we adopt.

### MLS Ciphersuite

**`MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519`** (OpenMLS ciphersuite 0x004D) — MLS (RFC 9420) with the PQ ciphersuites from `draft-ietf-mls-pq-ciphersuites`. See [MLS](/design/cryptography/mls/) for how the ciphersuite's choices (X-Wing KEM, ChaCha20-Poly1305 control AEAD, SHA-256 hash, Ed25519 leaf sigs) interact with the identity layer.

### Randomness

All keys, salts, and nonces are drawn from the operating system CSPRNG (`getrandom`). Capsule never seeds its own PRNG.

Nonces are never hand-rolled. The [STREAM construction](/design/cryptography/encryption/#stream-construction) derives per-chunk nonces deterministically; standalone [bulk-AEAD](#bulk-aead) metadata blobs each receive a fresh random nonce.

## Validation

Per-primitive verification is straightforward unit-test work:

- **Known-answer parity** against RFC test vectors and the well-known implementations (libsignal, OpenMLS, RustCrypto vectors). Every primitive ships with its vector set.
- **Suite-id round-trip** — encrypt/sign under suite `0x0001`, persist, re-read; the decoded `crypto_suite_id` must dispatch to exactly the row in the table. A test that asserts two suite ids cannot coexist except via a new row is the structural guard against accidental SSoT drift.
- **Downgrade-rejection** — attempt to verify a manifest whose declared `crypto_suite_id` differs from the value inside its signed envelope. Must reject.

Cross-doc test linkage: this doc owns *what is correct*; [Provenance](/design/cryptography/provenance/) owns *what `verify_asset` does with it*; [Threat Model — Validation](/design/threat-model/validation/) owns *what a key-less server rejects up front*.
