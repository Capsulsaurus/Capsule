---
title: Cryptography
description: Capsule's cryptographic stack — the entry point to the sub-docs
status: draft
---

Every primitive and key-handling routine lives in `capsule-core::crypto`; all clients and the server's no-key validation path run the same code.

| Sub-doc | Owns |
| --- | --- |
| [Primitives](/design/cryptography/primitives/) | Inventory of every hash, KDF, AEAD, signature, KEM, ciphersuite and version id |
| [Keys](/design/cryptography/keys/) | Key hierarchy, account types, device directory, write authorization, rotation |
| [MLS](/design/cryptography/mls/) | Group membership, history delivery, FS/PCS |
| [Encryption](/design/cryptography/encryption/) | Asset and metadata AEAD, wire formats |
| [Provenance](/design/cryptography/provenance/) | Signed manifests, append-only chains, derivative provenance |
| [Failure Modes](/design/cryptography/failure-modes/) | Failure catalog, recovery paths, transport security |

## Invariants

- **Audited libraries only** (RustCrypto, dalek, OpenMLS over libcrux); Capsule is never the first serious user of an implementation.
- **Memory hygiene:** plaintext and key material are zeroed on drop; secure allocation where available.
- **Server stores, never authorizes:** it holds ciphertext and key-free index facts; every authorization is verified against MLS-distributed material.
