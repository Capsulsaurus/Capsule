---
title: Cryptographic Primitives
description: Single-source-of-truth inventory of every cryptographic primitive Capsule uses
status: draft
---

Single source of truth for primitives; other docs link here, never restate. Constants: `capsule-core/src/crypto/primitives.rs`.

## Primitives Inventory

| Primitive | Choice | Used for |
| --- | --- | --- |
| [Cryptographic hash](#cryptographic-hash) | SHA-256 | Content addressing, integrity |
| [Key derivation](#key-derivation) | HKDF-SHA512 | Per-file, per-blob, per-album keys |
| [Password-based KDF](#password-based-kdf) | Argon2id, device-tier parameters | Escrow and backup unwrap |
| [Bulk AEAD](#bulk-aead) | AES-256-GCM (+ [STREAM](/design/cryptography/encryption/#stream-construction)) | Asset and metadata ciphertext |
| [MLS control AEAD](#mls-control-aead) | ChaCha20-Poly1305 | MLS's own messages |
| [Signature scheme](#signature-scheme) | Hybrid Ed25519 + ML-DSA-65 | Identity, device, manifest, write tier |
| [KEM](#kem) | X-Wing (X25519 + ML-KEM-768) | MLS HPKE |
| [MLS ciphersuite](#mls-ciphersuite) | `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`) | Group key management |
| [Randomness](#randomness) | OS CSPRNG (`getrandom`) | All keys, salts, nonces |
| [Transport](/design/cryptography/failure-modes/#transport-security) | TLS 1.3, hybrid X25519+ML-KEM | Client–server, server–server |

## Versioning Identifiers

Mismatches **fail closed**:

| Identifier | Type | Owner | Carried in |
| --- | --- | --- | --- |
| `crypto_suite_id` | `u16` | this doc | every [AssetManifest](/design/cryptography/provenance/#asset-manifest), [metadata blob](/design/cryptography/encryption/#metadata-blob-wire-format), backup [MANIFEST.cbor](/design/backup-recovery/) |
| `protocol_version` | `YYYY-MM-DD` | [Versioning](/design/versioning/) | every AssetManifest, every request ([negotiation](/design/threat-model/validation/#protocol-and-capability-negotiation)), the album's MLS pin |
| `sidecar_schema` | `u16` | [Metadata](/design/metadata/#sidecar-schema-v1) | CBOR sidecar field 0 |

- `crypto_suite_id = 0x0001` is exactly the inventory above. Retiring a primitive adds a new suite id; it never edits the existing row. Old suite ids stay verifiable and decryptable forever.
- Suite ids are a Capsule namespace, unrelated to MLS ciphersuite codepoints.
- Manifest signatures cover `crypto_suite_id` and `protocol_version`, so a downgrade cannot be produced silently.

## Per-Primitive Choices

### Cryptographic Hash

SHA-256 everywhere; the content-address hash is the value the [signed manifest](/design/cryptography/provenance/#asset-manifest) commits to and upload verifies.

### Key Derivation

HKDF-SHA512 (wider hash keeps ~256-bit margin under Grover). Every derivation MUST use a versioned `info` string and a scope-unique salt.

### Password-based KDF

Argon2id, 32-byte random salt, recovery/bootstrap only. Cost is chosen by device tier at **wrap** time (`DeviceTier::params`), stored in-band, and honoured at unwrap on any device.

### Bulk AEAD

AES-256-GCM (hardware-accelerated on every target). Nonce misuse is closed structurally: a fresh per-encryption file key for STREAM, a fresh CSPRNG nonce per metadata blob ([Re-keying on Rewrite](/design/cryptography/encryption/#re-keying-on-rewrite)).

### MLS Control AEAD

Fixed by the [MLS ciphersuite](#mls-ciphersuite); MLS messages only, never user data.

### Signature Scheme

| Signature | Scheme |
| --- | --- |
| Long-lived identity/evidence: user IK, device keys, manifests, write-tier keys, [custody receipts and storage attestations](/design/transfer/durability/#custody-receipts) | Hybrid Ed25519 + ML-DSA-65; **both** halves MUST verify |
| Short-lived operational: server–server federation, [federation capabilities](/design/federation/#federation-capabilities), [access-token JWTs](/design/authentication/#access-token) | Ed25519 only |
| MLS LeafNode | Ed25519 (ciphersuite-pinned); the ML-DSA half lives at the identity layer ([MLS](/design/cryptography/mls/)) |

The boundary is signature **lifetime**, not signer: a server-signed custody receipt is long-lived evidence, so it is hybrid.

### KEM

Defined by the MLS ciphersuite.

### MLS Ciphersuite

`0x004D`, the X-Wing suite from `draft-mahy-mls-xwing` that OpenMLS ships via libcrux — not `draft-ietf-mls-pq-ciphersuites`. Its SHA-256 is MLS-internal and independent of the content-address hash. Rationale and migration posture: [MLS](/design/cryptography/mls/).

### Randomness

OS CSPRNG only; never a self-seeded PRNG or hand-rolled nonce.

## Validation

- Known-answer vectors (RFC, RustCrypto, OpenMLS) for every primitive.
- Suite-id round-trip: persisted `crypto_suite_id` dispatches to exactly its row.
- Downgrade rejection: a manifest whose declared suite differs from its signed envelope is rejected.
