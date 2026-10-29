---
title: Asset and Metadata Encryption
description: How Capsule encrypts asset bytes and metadata blobs, including streaming and wire formats
status: draft
---

All asset bytes (originals, derivatives) and metadata blobs are encrypted client-side before crossing a network boundary. `capsule-core::crypto::encryption` is the only caller of AES-256-GCM. Two constructions:

| Payload | Construction | Why |
| --- | --- | --- |
| Asset bytes | [STREAM](#stream-construction) | Large, streamed, ranged reads, per-chunk authentication |
| Metadata blob | Standalone AEAD, [fixed wire format](#metadata-blob-wire-format) | Small, always fetched whole, content-addressed |

## Authenticated Asset Encryption

An asset is content-addressed by the hash of its ciphertext and encrypted under a per-encryption file key derived from an [AMK](/design/cryptography/keys/#album-master-keys-amks).

### Asset Key Derivation

- An album carries a versioned AMK ledger (`AMK_v1..v_n`, random 32 bytes each, new version per epoch bump), distributed over MLS ([History Delivery](/design/cryptography/mls/#history-delivery-for-new-joiners)) and stored hardware-wrapped on each member device. Keys MUST NOT be derived from MLS epoch internal state.
- **Derived mode** (default): `file_key = HKDF-SHA512(ikm = AMK_v{amk_version}, salt = file_id || nonce_prefix, info = "asset-file/v1", L = 32)`. Never stored.
- `nonce_prefix` is a fresh CSPRNG draw on **every** encryption, so the file key is unique per encryption even for the same `file_id` and epoch. This is what lets the STREAM counter start at zero without nonce reuse.
- **Wrapped mode** (external-origin assets only, e.g. an adopted [web-upload drop](/design/web-upload/)): the guest's random key `K` cannot be re-derived, so it is carried as `wrapped_file_key = wrap_nonce || AES-256-GCM(wrap_key, K)` with `wrap_key = HKDF-SHA512(AMK_v{n}, salt = file_id || wrap_nonce, info = "asset-keywrap/v1")`, recorded in the signed [manifest](/design/cryptography/provenance/#asset-manifest) as `key_mode = wrapped`. Only the adopting `create` sets it; any later `replace` re-encrypts under a derived key. This is the only stored per-file key (see [Key Chain](/design/cryptography/keys/#key-chain)).
- **Distribution lag is not failure.** A manifest whose `amk_version` is within the [MLS-attested epoch range](/design/cryptography/keys/#write-authorization) but whose AMK has not arrived is *pending* and retried. Only a version beyond the attested epoch, or still missing after the retry timeout, is escalated ([Failure Modes](/design/cryptography/failure-modes/#failure-mode-catalog)).

### Encryption Workflow

1. Derive `file_key` from the current AMK with a fresh 7-byte `nonce_prefix`.
2. STREAM-encrypt; hash the ciphertext incrementally (algorithm fixed by `crypto_suite_id`).
3. Build and sign the [manifest](/design/cryptography/provenance/#asset-manifest) (device + write-tier signatures).
4. Upload via the [Transfer — Upload](/design/transfer/upload/).

An upload re-derives each ciphertext range from the plaintext on disk, so the plaintext MUST NOT change under it. A changed plaintext re-encrypted under the same file key and nonce would hand the server two ciphertexts under one AES-GCM nonce, so two guards stand before any re-derived byte is sent:

- **Source identity**, best-effort: building the bundle records the source's size, modification time and file identifier, and every later read checks them first. A file system with coarse timestamps, or a writer that preserves them, can hide a change.
- **Chunk tags**, authoritative: the bundle's hashing pass records every STREAM chunk's authentication tag and persists them with the bundle for as long as any of its sessions is open. A re-derived chunk whose tag differs is never sent. A range is sent only from whole STREAM chunks that have been re-derived and checked, even where it starts or ends inside one.

Either mismatch aborts the blob with a typed source-changed error before that read's bytes are sent. The finalize-time content-address check remains the backstop: a changed source can cost an upload, never store bytes under the wrong address.

Ranged read of plaintext byte `B`: `i = B / plaintext_chunk`; fetch ciphertext at offset `i × ciphertext_chunk` (chunk + 16-byte tag). Chunk `i` decrypts independently. Using the plaintext stride as the ciphertext offset is a bug; the formula is normative.

### Re-keying on Rewrite

`replace`, `derivative-replace` and `metadata-update` ([actions](/design/authorization/#the-closed-action-set)) keep the same id and may land in the same epoch. The fresh nonce folded into the KDF salt re-rolls **both** key and nonce, so identical plaintext yields different ciphertext.

- MUST NOT reuse a prior `(key, nonce)` pair.
- The writer refuses a `nonce_prefix` / metadata nonce it has already emitted for that id (defense in depth over the CSPRNG).

### STREAM Construction

AES-256-GCM under STREAM (Hoang–Reyhanitabar–Rogaway–Vizár 2015) via RustCrypto `aead::stream::{EncryptorBE32, DecryptorBE32}<Aes256Gcm>`. Nonce = `prefix(7) || counter(4) || last_flag(1)`; detects truncation, reordering and chunk deletion. Plaintext chunk 65,520 B → 64 KiB ciphertext chunk. The upload transport's chunk alignment is a separate concern.

## Metadata Encryption

| Data | Server view |
| --- | --- |
| CBOR metadata blob (incl. [LQIP](/design/thumbnails/#lqip), `dominant_color`) | Encrypted under the AMK; versioned and signed like a manifest |
| `owner_id`, ciphertext hash, ciphertext size | Plaintext — routing and quota need them |
| AI embeddings (search, faces) | Plaintext locally only; always encrypted at rest on the server |

The blob plaintext is the signed [`SidecarV1`](/design/metadata/#sidecar-schema-v1) in [canonical CBOR](/design/metadata/#canonical-cbor-encoding). Deterministic encoding is mandatory: the blob hash is signed, so encoder drift breaks cross-peer verification.

### Metadata Blob Wire Format

Implementations MUST produce and consume exactly:

```text
crypto_suite_id (u16 BE) || nonce (12, fresh CSPRNG) || AES-256-GCM ciphertext || tag (16)
```

- `crypto_suite_id` MUST equal the manifest's ([Versioning Identifiers](/design/cryptography/primitives/#versioning-identifiers)); mismatch rejects at decode.
- Key: `HKDF-SHA512(AMK_v{n}, salt = blob_id || nonce, info = "metadata-blob/v1", L = 32)`.
- The content hash covers the whole byte string and is committed as `metadata_blob_hash` on every `create`, `replace` and `metadata-update`, under both manifest signatures.
- **Local–server equivalence:** on decode the plaintext MUST equal the locally stored signed sidecar byte-for-byte and the hash MUST equal `metadata_blob_hash`; otherwise [quarantine](/design/threat-model/validation/#client-side-validation-invariants). See [Metadata](/design/metadata/#local-and-server-metadata-equivalence).

## Validation

- Round-trip and fixed vectors for STREAM and metadata AEAD.
- STREAM tamper: bit flip, swap, drop, last-flag toggle all rejected.
- Ranged read of chunk `i` matches the plaintext slice; stitched ranges equal sequential decrypt.
- Metadata blob determinism against the shared canonical-CBOR vectors — a **blocking** cross-language gate.
- Rewrite re-roll: same id and epoch yields different key and nonce; reused nonce refused.
- Sidecar → blob → sidecar is byte-identical and hash-matches; a one-byte local mutation fails.
