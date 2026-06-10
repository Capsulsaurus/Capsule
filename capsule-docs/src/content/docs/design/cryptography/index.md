---
title: Cryptography
description: Capsule's cryptographic stack — the entry point to the sub-docs
---

Cryptography in Capsule is everything that makes E2EE work over an asset-heavy, sync-heavy workload. The choices and constructions are split across focused sub-docs because each is implementable and testable on its own, but they share one home: every primitive and key-handling routine lives in `capsule-core::crypto`, so every client and the server's no-key envelope-validation path use exactly the same code.

## End-to-End Model in Layers

Capsule's E2E security stacks four layers, each owned by its own sub-doc:

- **Identity** — per-device keys, cross-signed by a user master identity. See [Keys](/design/cryptography/keys/).
- **Group membership** — one MLS group per shared album; each device is a leaf. See [MLS](/design/cryptography/mls/).
- **Asset encryption** — bulk AEAD per file, keyed via the album-scoped KDF. See [Encryption](/design/cryptography/encryption/).
- **Metadata encryption** — bulk AEAD per metadata blob, keyed the same way. (No streaming construction; metadata is fetched whole.) See [Encryption](/design/cryptography/encryption/).

## Sub-docs

| Sub-doc                                              | Owns                                                                                                                |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| [Primitives](/design/cryptography/primitives/)       | **SSoT primitives inventory** — every hash, KDF, AEAD, signature scheme, KEM, ciphersuite, and version identifier   |
| [Keys](/design/cryptography/keys/)                   | Key hierarchy (master, device, AMK, write-tier), account-type key derivation, device directory, rotation/revocation |
| [MLS](/design/cryptography/mls/)                     | Group membership protocol, ciphersuite binding, history delivery, FS/PCS                                            |
| [Encryption](/design/cryptography/encryption/)       | Asset AEAD (AES-256-GCM-STREAM), metadata AEAD, deterministic CBOR encoding, wire formats                           |
| [Provenance](/design/cryptography/provenance/)       | Signed manifests, append-only provenance chains, derivative provenance                                              |
| [Failure Modes](/design/cryptography/failure-modes/) | Failure-mode catalog, 7 independent recovery paths, bug-resistance invariants, transport security                   |

## Implementation Posture

- **Centralized.** All cryptographic primitives, key handling, and `verify_asset` live in `capsule-core::crypto`. There is no per-platform divergence in what gets verified or how — only in where keys are physically held.
- **Audited libraries only.** libcrux (formally verified), RustCrypto, ed25519-dalek, x25519-dalek, OpenMLS. Capsule is never the first serious user of a primitive's implementation.
- **Memory hygiene.** Decrypted bytes and key material are zeroed on drop; secure-allocation is used where the platform supports it, to prevent swap leaks.
- **Trust the server for storage, never for authorization.** The server holds opaque ciphertext and key-free index facts. Every authorization is verified against MLS-distributed material; a server's assertion of access is never sufficient.
