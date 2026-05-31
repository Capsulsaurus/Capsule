---
title: Asset and Metadata Encryption
description: How Capsule encrypts asset bytes and metadata blobs, including streaming and wire formats
---

Every asset Capsule stores — original bytes, derivative bytes, metadata blob — is encrypted client-side before it ever crosses a network boundary. The encryption code lives in `capsule-core::crypto::encryption` and is the only place AES-256-GCM is invoked in the codebase. Two constructions live here:

- **STREAM** for asset bytes (originals + derivatives) — supports streaming, ranged reads, and per-chunk authentication.
- **Standalone AEAD** for metadata blobs — a single contiguous byte string with a fixed wire format.

The split is intentional: assets are huge and accessed in pieces; metadata blobs are small and always fetched whole.

## Authenticated Asset Encryption

Every asset is content-addressed by the SHA-256 of its ciphertext and encrypted with a unique file key. The file key is derived from the appropriate [AMK](/design/cryptography/keys/#album-master-keys-amks); the AMK itself is recoverable from the account's master key (see [Identity-Based Key Derivation](/design/cryptography/keys/#identity-based-key-derivation)).

### Asset Key Derivation

Each asset is encrypted with a key derived from a versioned album master key (AMK), distributed and ledgered over MLS (see [MLS](/design/cryptography/mls/)). Capsule never derives a key from the MLS epoch's internal state.

An album's AMK ledger:

```rust
Album {
  id: UUID,
  mls_group: MlsGroup,
  keys: [
    AMK_v1: (random 32 bytes, created at album creation),
    AMK_v2: (random 32 bytes, created when member X was removed),
    AMK_v3: ...
  ],
  current_version: 3,
}
```

The per-file key is derived from the AMK version that encrypted it, using the [KDF](/design/cryptography/primitives/#key-derivation):

```rust
file_key = HKDF_SHA512(
  ikm: AMK_v{amk_version},
  salt: file_id,
  info: "asset-file/v1",
  length: 32        // 32 bytes for AES-256; HKDF-SHA512 expand truncates safely
)
```

AMKs are delivered over MLS application messages. When epoch N's MLS group is established, the creating device sends an `AlbumKeyDistribution { amk_version, amk_bytes }` message through MLS. Every current member's device receives and stores it locally (hardware-wrapped).

**Distribution lag is expected and is not a failure.** An epoch bump and its `AlbumKeyDistribution` broadcast are separate MLS messages, so during a bump a device can legitimately receive an asset manifest referencing an `amk_version` whose key bytes have not yet arrived. A device that lacks the AMK for an `amk_version` that is otherwise **within the [MLS-attested epoch range](/design/cryptography/keys/#write-authorization)** treats the asset as *pending* — held and retried as MLS state catches up — rather than as a decryption failure or a forged manifest. Only an `amk_version` beyond the MLS-attested epoch, or one still missing after the retry timeout, is escalated. This is the `verify_asset` *pending* outcome and the matching [Failure Modes](/design/cryptography/failure-modes/#failure-mode-catalog) row; it is what keeps a concurrent upload during an epoch bump from being misread as an attack.

### Encryption Workflow

Encrypting an asset for upload:

1. Derive `file_key` from `AMK_v{current}` (above).
2. Generate a random 7-byte `nonce_prefix` from the OS CSPRNG (7 = the 12-byte AES-GCM nonce minus STREAM's 4-byte chunk counter and 1-byte last-chunk flag).
3. Split the plaintext into 65,520-byte chunks and encrypt sequentially with `EncryptorBE32<Aes256Gcm>`, producing 64 KiB ciphertext chunks (16-byte tag each); the final chunk is flagged as last.
4. Compute the `ciphertext_hash` incrementally over the produced ciphertext (algorithm fixed by `crypto_suite_id`).
5. Build and sign the [manifest](/design/cryptography/provenance/#asset-manifest) (device signature + write-tier signature).
6. Upload the blob (see [Upload Protocol](/design/import/upload-protocol/)).

Streaming download / ranged reads:

- **Sequential:** `DecryptorBE32<Aes256Gcm>` consumes chunks in order, verifying each tag.
- **Ranged:** to start at plaintext byte `B`, compute `chunk_index = B / 65,520`. Because the [STREAM construction](#stream-construction) derives each chunk's nonce deterministically, chunk `i` decrypts independently given `file_key` and `i` — the server need only serve that 64 KiB ciphertext chunk, which the client decrypts and verifies.

### STREAM Construction

Capsule strictly requires streaming.

The chosen method is AES-256-GCM with the STREAM construction (Hoang-Reyhanitabar-Rogaway-Vizár, 2015). STREAM splits the file into chunks, encrypts each with AES-GCM using a structured nonce (`prefix || counter || last-chunk-flag`), and guarantees you detect truncation, reordering, and chunk deletion.

In Rust: the RustCrypto `aead` crate exposes `stream::EncryptorBE32<Aes256Gcm>` and `stream::DecryptorBE32<Aes256Gcm>` — drop-in. We use a 65,520-byte plaintext chunk → 64 KiB ciphertext chunk. (Note the upload transport's 4 KiB chunk alignment, described in [Upload Protocol](/design/import/upload-protocol/), is a separate concern from this crypto chunk size.)

## Metadata Encryption

Not all metadata can be encrypted — some must stay server-readable for routing and preview. The split is deliberate:

- **Encrypted** (AES-256-GCM under a key derived from the album's AMK, fresh random nonce per blob): the CBOR sidecar / metadata blobs — including the [chromahash LQIP](/design/thumbnails/#lqip) and `dominant_color`, so image-derived display hints never leak to a server that never decodes assets. Each blob is independently versioned and signed like an [asset manifest](/design/cryptography/provenance/#asset-manifest).
- **Server-plaintext by necessity:** `owner_id`, the [ciphertext content hash](/design/cryptography/primitives/), and the ciphertext size — the routing and storage-accounting facts a key-less server needs. This is a deliberate, documented trade-off.
- **AI embeddings** (semantic-search vectors, face embeddings) are sensitive — a user can be re-identified from them. They are kept plaintext *locally* (vector search requires it) but encrypted at rest in the server-side backup.

CBOR metadata blobs use **deterministic encoding** per the [canonical CBOR ruleset](/design/metadata/#canonical-cbor-encoding) owned by [Metadata](/design/metadata/) — the same byte-exact rules the plaintext sidecar follows, since the metadata blob's plaintext *is* that CBOR document. Because a blob's hash is what content-addresses it and what the [signed manifest](/design/cryptography/provenance/#asset-manifest) commits to, two implementations encoding the same logical metadata must produce byte-identical output — otherwise the hash diverges and the signature fails to verify across [federated](/design/federation/) peers. Conformance to the canonical ruleset is mandatory and is the load-bearing check behind cross-platform and cross-language interop.

### Metadata Blob Wire Format

An encrypted metadata blob is a single contiguous byte string. **Implementations MUST produce and consume exactly this layout**, with no framing variations, so two correct implementations can compute identical content hashes byte-for-byte. This wire format is itself the contract: any byte-level deviation breaks cross-peer signature verification.

```text
+---------------------+---------------------+--------------------------+---------------+
| crypto_suite_id (2) | nonce (12 bytes)    | ciphertext (variable)    | tag (16 bytes)|
+---------------------+---------------------+--------------------------+---------------+
| big-endian u16      | fresh CSPRNG draw   | AES-256-GCM(plaintext)   | GCM tag       |
```

- `crypto_suite_id` (2 bytes, big-endian `u16`) — pins the AEAD and KDF used to derive the key. Identical to the field carried inside the manifest (see [Versioning Identifiers](/design/cryptography/primitives/#versioning-identifiers)), and a mismatch with the manifest's value rejects the blob at decode.
- `nonce` (12 bytes) — fresh OS-CSPRNG per blob; never reused, never derived.
- `ciphertext` — the deterministically-encoded CBOR plaintext, sealed with AES-256-GCM under `HKDF-SHA512(ikm=AMK_v{n}, salt=blob_id, info="metadata-blob/v1", length=32)`.
- `tag` (16 bytes) — GCM authentication tag.

The total blob's `ciphertext_hash` (in the asset's [signed manifest](/design/cryptography/provenance/#asset-manifest)) is computed over the full byte string above — header, nonce, ciphertext, and tag concatenated.

## Validation

- **Encrypt-decrypt round-trip** — for both STREAM and standalone metadata AEAD, unit tests that randomized plaintext bytes encrypt and decrypt to themselves. Fixed-vector cases pin the per-primitive parameters.
- **STREAM tamper-detection** — unit tests that mutate each chunk in turn (single bit flip, chunk swap, chunk drop, final-chunk-flag toggle) and assert `DecryptorBE32` rejects.
- **Ranged-read correctness** — unit test that fetching chunk `i` in isolation decrypts to the matching plaintext slice (no off-by-one), and that ranged reads stitched together byte-match a sequential decrypt.
- **Metadata blob wire-format determinism** — cross-language conformance test (Rust ↔ any FFI consumer) that encoding the same logical CBOR map produces byte-identical blobs against the shared [canonical CBOR known-answer vectors](/design/metadata/#canonical-cbor-encoding). This is a **blocking conformance gate**, not advisory: a consumer that drifts cannot be shipped, because its signatures would not verify across peers.
- **Nonce-misuse refusal** — unit test that the metadata-blob writer rejects an attempt to reuse a previously-emitted nonce (defense in depth on top of the CSPRNG fresh-draw rule).

Wire-format compatibility with the upload protocol is exercised by [Upload Protocol](/design/import/upload-protocol/) smoke tests; this doc's responsibility is the byte-level correctness of the AEAD itself.
