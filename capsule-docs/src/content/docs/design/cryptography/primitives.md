---
title: Cryptographic Primitives
description: Single-source-of-truth inventory of every cryptographic primitive Capsule uses
audit: none
---

The single source of truth for primitives: other docs link here and never restate. Constants live in `capsule-core/src/crypto/primitives.rs`.

## Primitives Inventory

| Primitive | Choice | Used for |
| --- | --- | --- |
| [](#cryptographic-hash) | SHA-256 | Content addressing, integrity |
| [](#key-derivation) | HKDF-SHA512 | Per-file, per-blob, per-album keys |
| [](#password-based-kdf) | Argon2id, device-tier parameters | Escrow and backup unwrap |
| [](#bulk-aead) | AES-256-GCM (+ STREAM, [](/design/cryptography/encryption/#stream-construction)) | Asset and metadata ciphertext |
| [](#mls-control-aead) | ChaCha20-Poly1305 | MLS's own messages |
| [](#signature-scheme) | Hybrid Ed25519 + ML-DSA-65 | Identity, device, manifest, write tier |
| [](#kem) | X-Wing (X25519 + ML-KEM-768) | MLS HPKE |
| [](#mls-ciphersuite) | `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`) | Group key management |
| [](#randomness) | OS CSPRNG (`getrandom`) | All keys, salts, nonces |
| [](/design/networking/#transport-security) | TLS 1.3, hybrid X25519+ML-KEM | Client–server, server–server |

## Versioning Identifiers

Mismatches **fail closed**.

| Identifier | Type | Owner | Carried in |
| --- | --- | --- | --- |
| `crypto_suite_id` | `u16` | this doc | every AssetManifest ([](/design/cryptography/provenance/#asset-manifest)), every blob header ([](/design/cryptography/encryption/#blob-header)), backup MANIFEST.cbor ([](/design/backup-recovery/)) |
| blob `format_version` | `u8` | [](/design/cryptography/encryption/#blob-header) | every blob header |
| `protocol_version` | `YYYY-MM-DD` | [](/design/versioning/) | every AssetManifest, every request (negotiation: [](/design/threat-model/validation/#protocol-and-capability-negotiation)), the album's MLS pin |
| `sidecar_schema` | `u16` | [](/design/metadata/#sidecar-schema-v1) | CBOR sidecar field 0 |

- `crypto_suite_id = 0x0001` is exactly the inventory above. Retiring a primitive adds a new suite id and never edits the existing row. Old suite ids stay verifiable and decryptable forever.
- Suite ids are a Capsule namespace, unrelated to MLS ciphersuite codepoints.
- Manifest signatures cover `crypto_suite_id` and `protocol_version`, so a downgrade cannot be produced silently.
- A blob's `suite_id` is authenticated as AEAD associated data, so it cannot be swapped even where no manifest is at hand ([](/design/cryptography/encryption/#header-as-aad)).

## Per-Primitive Choices

### Cryptographic Hash

SHA-256 everywhere. The content-address hash is the value the signed manifest commits to and upload verifies ([](/design/cryptography/provenance/#asset-manifest)).

### Key Derivation

HKDF-SHA512: the wider hash keeps a ~256-bit margin under Grover. Every derivation MUST use a versioned `info` string and a scope-unique salt.

### Password-based KDF

Argon2id with a 32-byte random salt, for recovery and bootstrap only. Cost is chosen by device tier at **wrap** time (`DeviceTier::params`), stored in-band, and honoured at unwrap on any device **within fixed bounds**.

The parameters arrive from places the unwrapping device does not control: the server serves the escrow and share-link wrapped secrets, and a link issuer mints a drop passphrase. Unchecked, a hostile blob declaring gigabytes of memory denies recovery by getting the app killed, with no signature or hash failing. So:

| Parameter | Floor | Ceiling |
| --- | --- | --- |
| Memory `m` | 128 MiB (the `LowRam` tier) | 1 GiB |
| Iterations `t` | 3 | 8 |
| Parallelism `p` | 1 | 4 |

- **Wrap** refuses parameters below the floor or above the ceiling.
- **Unwrap** checks the declared parameters against both bounds *before* running Argon2id and refuses out-of-bounds ones with a typed `params out of bounds` error, distinct from a wrong passphrase, so the user is never told to retype a passphrase that was right.
- The floor is consistency, not the brute-force defence: rewriting the declared cost of an existing blob only yields a wrong key, because the attacker cannot re-wrap a secret it does not hold. Brute-force resistance is fixed by the cost the honest writer chose.
- Raising the ceiling is a release decision, made in this table; a writer never emits parameters an older reader in the supported window would refuse ([](/design/versioning/#update-policy)).

### Bulk AEAD

AES-256-GCM, hardware-accelerated on every target. Nonce misuse is closed structurally: STREAM gets a fresh file key per encryption and each metadata blob a fresh CSPRNG nonce ([](/design/cryptography/encryption/#re-keying-on-rewrite)). GCM is not key-committing, so every blob carries an HKDF key commitment and keys AES with a subkey of its content key ([](/design/cryptography/encryption/#key-commitment)).

### MLS Control AEAD

Fixed by the [](#mls-ciphersuite). MLS messages only, never user data.

### Signature Scheme

- **Hybrid Ed25519 + ML-DSA-65, both halves MUST verify:** long-lived identity and evidence, i.e. user IK, the recovery key ([](/design/cryptography/keys/#identity-rotation)), device keys, manifests, write-tier keys, custody receipts and storage attestations ([](/design/transfer/durability/#custody-receipts)).
- **Ed25519 only:** short-lived operational signatures, i.e. server–server federation, federation capabilities ([](/design/federation/#federation-capabilities)), access-token JWTs ([](/design/authentication/#access-token)).
- **Ed25519, ciphersuite-pinned:** the MLS LeafNode. The ML-DSA half lives at the identity layer ([](/design/cryptography/mls/)).

The boundary is signature **lifetime**, not signer: a server-signed custody receipt is long-lived evidence, so it is hybrid.

### KEM

Defined by the MLS ciphersuite.

### MLS Ciphersuite

`0x004D`, the X-Wing suite from `draft-mahy-mls-xwing` that OpenMLS ships via libcrux, not `draft-ietf-mls-pq-ciphersuites`. Its SHA-256 is MLS-internal and independent of the content-address hash. Rationale and migration posture are in [](/design/cryptography/mls/).

### Randomness

OS CSPRNG only, never a self-seeded PRNG or hand-rolled nonce.

## Known Gaps

The Argon2id bounds are not enforced: `capsule-core/src/crypto/pwkdf.rs` runs whatever parameters a wrapped secret declares, and wrap accepts any.

## Validation

- Known-answer vectors (RFC, RustCrypto, OpenMLS) for every primitive.
- Suite-id round-trip: a persisted `crypto_suite_id` dispatches to exactly its row.
- Downgrade rejection: a manifest whose declared suite differs from its signed envelope is rejected.
- Argon2id bounds: a wrapped secret one step outside each floor and ceiling is refused before any Argon2id work, with the bounds error, on every platform including the browser.
