---
title: Asset and Metadata Encryption
description: How Capsule encrypts asset bytes and metadata blobs, the blob header and key commitment, and how a reader classifies a blob that will not open
audit: none
---

All asset bytes (originals, derivatives) and metadata blobs are encrypted client-side before crossing a network boundary. `capsule-core::crypto::encryption` is the only caller of AES-256-GCM for user data. Every encrypted blob is one [](#blob-header) followed by the output of one of two constructions:

| Payload                                                                | Construction                                      | Why                                                     |
| ---------------------------------------------------------------------- | ------------------------------------------------- | ------------------------------------------------------- |
| Asset bytes (original, derivative)                                     | [](#stream-construction)                          | Large, streamed, ranged reads, per-chunk authentication |
| Metadata blob, signed album and group documents, share metadata record | Single-shot AEAD ([](#metadata-blob-wire-format)) | Small, always fetched whole, content-addressed          |

A **blob** here is any encrypted byte string stored under its content address. Key envelopes (the wrapped file key, the drop KEM-DEM wrap, the share scope wrap, the master-key seal, the password-wrapped secret) are fields inside a signed or server record, not blobs, and keep their compact formats; see [](#key-envelopes).

## Blob Header

Every blob begins with a fixed 56-byte header, and the header is identical in layout for both constructions:

```text
offset len field          value
0      4   magic          0x89 'C' 'P' 'B'
4      1   format_version 0x01
5      1   kind           0x01 = STREAM, 0x02 = single-shot
6      2   suite_id       crypto_suite_id, u16 BE
8      4   key_epoch      amk_version of the AMK the key is derived from, u32 BE; 0 = not AMK-derived
12     12  nonce          STREAM: nonce_prefix(7) || 0x00 x 5; single-shot: the GCM nonce
24     32  commitment     key commitment, see below
```

- **Read order.** A reader checks `magic`, then `format_version`, then `kind` and `suite_id`, before it touches anything else. An unknown value of any of them is _unsupported_ ([](#reading-a-blob)), never a guess. The leading `0x89` is outside ASCII, so a text-mode or 7-bit mangling breaks the magic rather than yielding a plausible header.
- **`format_version` fixes the layout and the STREAM geometry.** Version 1 means this header, 65,520-byte plaintext chunks and the AAD rule below. Changing any of them is a new `format_version`; changing a primitive is a new `suite_id` ([](/design/cryptography/primitives/#versioning-identifiers)).
- **Reserved bytes are zero.** A STREAM header whose nonce bytes 7–11 are non-zero is malformed.
- **`key_epoch`** names which AMK the reader needs, so _key not held_ is decidable from the blob. It is `key_mode = wrapped ? 0 : amk_version` for an asset blob ([](#asset-key-derivation)). A blob whose key is not derived from an AMK carries `0`: a share metadata record, keyed from the link secret, and a wrapped-mode asset, whose key a guest drew before any AMK was involved. A reader of a wrapped asset takes the epoch from the manifest's `amk_version`, which names the AMK that unwraps `wrapped_file_key`. The server already sees `amk_version` in the manifest, so the field discloses nothing new.
- **The header is a deterministic function of the key, `kind`, `suite_id`, `key_epoch` and the nonce.** An upload that re-derives a ciphertext range from the plaintext ([](#encryption-workflow)) therefore re-derives the same header bytes.

### Key Commitment

AES-GCM is not key-committing: a wrong key and a badly sealed blob both surface as one failed tag, and a ciphertext can be built that opens under two different keys. The header closes both. Let `K` be the blob's content key (the file key of [](#asset-key-derivation), or the metadata-blob key of [](#metadata-blob-wire-format)) and `H24` the first 24 header bytes (everything before `commitment`). Then:

```text
aead_key   = HKDF-SHA512(ikm = K, salt = H24, info = "blob-aead/v1",   L = 32)
commitment = HKDF-SHA512(ikm = K, salt = H24, info = "blob-commit/v1", L = 32)
```

- `K` itself never keys AES. One HKDF extract feeds both expands, so the commitment costs one HMAC-SHA512 block per blob.
- Salting with `H24` binds the commitment to the suite, epoch and nonce: a header edited in any of them no longer matches its commitment.
- A reader MUST compare the commitment it derives with the header's in constant time **before** opening any chunk, and MUST NOT decrypt on a mismatch.
- The commitment is one-way in `K` and reveals nothing about it. Finding two keys with one commitment is a collision in a 256-bit HKDF output.
- The signed manifest mirrors every blob's commitment ([](/design/cryptography/provenance/#asset-manifest)), so a reader holding the manifest checks the key before it fetches a byte, and the server checks header against manifest without a key ([](#header-and-manifest-agreement)).

### Header as AAD

The full 56-byte header is the associated data of **every** AEAD call over the blob: of each STREAM chunk, and of the single-shot seal. A header changed after sealing therefore fails the first tag a reader checks, even on a ranged read that never hashes the whole blob. This is what authenticates `suite_id` in-band; a suite id that is merely compared against a manifest would leave a manifest-less reader (a backup's `meta/`, a share record, a delegation courier) with nothing binding it.

### Content Address

The content address is SHA-256 over the **whole blob, header included**. The header is part of the address, so it is covered by the manifest's `ciphertext_hash` and `metadata_blob_hash` and by every custody receipt over them, and a corrupted header is caught by the address before anything else reads it. A blob's length is `56 + ciphertext length` and the server's ciphertext size, quota and upload offsets count the header.

### Why a Header

The content address alone already detects corruption without a key: a fetch hashes the bytes against the address it asked for and discards a mismatch (`capsule-sdk/src/fetch.rs`, `FetchError::IntegrityFailed`). The header is not for corruption. It earns its 56 bytes for three things the address cannot do:

1. **Wrong-key diagnosis.** Without a commitment, a hash-correct blob that fails its tag could be a wrong or stale AMK, a forged key delivery or an uploader bug, and today's fetch path reports all of them as one `FetchError::Corrupt`. The commitment splits _wrong key_ from _sealed bad at source_, which have opposite remedies.
2. **Checks with no manifest pairing.** A delegation courier, a backup verifier or a share viewer can see a blob's format, suite and epoch from the blob alone, and the server can check that a bundle's blobs agree with its manifest keylessly.
3. **Authenticated suite dispatch.** The suite id is bound by AEAD, not only by a manifest comparison.

## Reading a Blob

A reader classifies a blob that will not open into exactly one state, in this order, and each state has one client action. The first failing test wins.

| #   | State                                 | Test                                                                                                                                          | Client action                                                                                                                                                                                                                                                                      |
| --- | ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | **Corrupt**                           | SHA-256 of the bytes differs from the expected content address                                                                                | Discard the bytes; re-fetch from another copy (path 6 of [](/design/backup-recovery/#recovery-paths)) and report the bad copy's holder. Never decrypt.                                                                                                                             |
| 2   | **Unsupported**                       | `magic` is right but `format_version`, `kind` or `suite_id` is unknown to this build                                                          | Keep the blob and its records untouched; prompt an upgrade (invariants 2 and 5 of [](/design/versioning/#system-invariants)). Never delete.                                                                                                                                        |
| 3   | **Sealed bad at source** (structural) | Address matches, but `magic` is wrong, reserved bytes are set, or the header disagrees with the manifest ([](#header-and-manifest-agreement)) | As state 6.                                                                                                                                                                                                                                                                        |
| 4   | **Key not held**                      | The AMK for `key_epoch` (for a wrapped asset, the AMK for the manifest's `amk_version`; for a share record, the share-scope key) is not held  | Within the attested epoch range: _pending_, fetch the key (AlbumKeyDistribution, album escrow) and retry ([](/design/cryptography/keys/#write-authorization)). Outside the album's history entitlement: a placeholder, not an error. Beyond the attested ceiling: terminal-reject. |
| 5   | **Wrong key**                         | The derived commitment differs from the header's                                                                                              | Do not decrypt. Re-obtain the AMK from the authority or the album escrow and retry once. A mismatch that survives the refresh is a key divergence: surface it and run the re-keying ceremony ([](/design/mls-resilience/#group-re-keying-ceremony)).                               |
| 6   | **Sealed bad at source**              | Commitment matches, a tag fails                                                                                                               | The bytes are exactly what the uploader produced, so a re-fetch cannot help. Mark the asset unreadable, surface it against `created_by_device`, and report it to the uploader, which re-seals from its plaintext with a `replace` if it still holds it.                            |

Rules:

- **Without an expected address** (a blob alone), test 1 cannot run: a wrong `magic` is then _corrupt_, not _sealed bad at source_.
- **A ranged read cannot classify a tag failure.** It has not hashed the whole blob, so a failed chunk tag may be corruption at rest. The reader MUST escalate to a whole-blob address check (or the server's storage verify, [](/design/transfer/durability/)) before it reports state 6, and treats the window as corrupt until then.
- **Discarded bytes are discarded.** A decrypting writer that has emitted plaintext from a blob later classified 1, 5 or 6 MUST discard that plaintext, as a streamed fetch already does for unverified bytes.
- The states are typed outcomes across the SDK, FFI and `--json` surfaces, never one undifferentiated "corrupt".

### Header and Manifest Agreement

A manifest-holding reader, and the server at finalize, check without any key that each blob's header agrees with the signed manifest that names it: `suite_id = crypto_suite_id`, `key_epoch = (key_mode = wrapped ? 0 : amk_version)`, the STREAM nonce prefix equals `nonce_prefix`, and `commitment` equals the mirrored commitment. The address matched, so a disagreement is the signer's own inconsistency: the server refuses the bundle and a client classifies it _sealed bad at source_.

## Authenticated Asset Encryption

An asset is content-addressed by the hash of its blob and encrypted under a per-encryption file key derived from an AMK ([](/design/cryptography/keys/#album-master-keys-amks)).

### Asset Key Derivation

- An album carries a versioned AMK ledger (`AMK_v1..v_n`, 32 bytes each, a new version per epoch bump), held on each member device and escrowed. How each epoch's AMK comes to exist, and who may deliver it, is owned by [](/design/cryptography/keys/#album-master-keys-amks).
- **Derived mode** (default): `file_key = HKDF-SHA512(ikm = AMK_v{amk_version}, salt = file_id || nonce_prefix, info = "asset-file/v1", L = 32)`. Never stored. The file key is the blob's content key `K`; the AES key is derived from it ([](#key-commitment)).
- `nonce_prefix` is a fresh CSPRNG draw on **every** encryption, so the file key is unique per encryption even for the same `file_id` and epoch. This lets the STREAM counter start at zero without nonce reuse.
- **Wrapped mode** is for external-origin assets only, e.g. an adopted web-upload drop ([](/design/web-upload/)). The guest's random key `K` cannot be re-derived, so it is carried as `wrapped_file_key = wrap_nonce || AES-256-GCM(wrap_key, K)` with `wrap_key = HKDF-SHA512(AMK_v{n}, salt = file_id || wrap_nonce, info = "asset-keywrap/v1")`, and recorded as `key_mode = wrapped` in the signed manifest ([](/design/cryptography/provenance/#asset-manifest)). The guest sealed its blob with `key_epoch = 0` and the adopter does not rewrite it, so the header keeps `0` and the manifest's `amk_version` names the unwrapping AMK ([](#blob-header)). Only the adopting `create` sets it; any later `replace` re-encrypts under a derived key. It is the only stored per-file key ([](/design/cryptography/keys/#key-chain)). The guest's blob carries a header like any other, so a wrong wrapped key is _wrong key_, not _sealed bad at source_.
- **Distribution lag is not failure.** A manifest whose `amk_version` is within the MLS-attested epoch range ([](/design/cryptography/keys/#write-authorization)) but whose AMK has not arrived is _pending_ and retried. Only a version beyond the attested epoch, or one still missing after the retry timeout, is escalated.

### Encryption Workflow

1. Derive `file_key` from the current AMK with a fresh 7-byte `nonce_prefix`, build the header and derive `aead_key` and the commitment.
2. Write the header, then STREAM-encrypt with the header as AAD, hashing header and ciphertext incrementally (algorithm fixed by `crypto_suite_id`).
3. **Open after seal.** Decrypt every chunk just produced, with the header as AAD and at the offsets a reader uses, and compare the plaintext digest with the source's. A failure aborts the write. The record of this check for the exact bytes the address names is what releasing the original rests on ([](/design/transfer/durability/#verify-before-destroy)); no server-side check can show that sealed bytes open ([](/design/threat-model/risks/)).
4. Build and sign the manifest, mirroring the commitment, with device and write-tier signatures ([](/design/cryptography/provenance/#asset-manifest)).
5. Upload via the upload protocol ([](/design/transfer/upload/)).

An upload re-derives each ciphertext range from the plaintext on disk, so the plaintext MUST NOT change under it: a changed plaintext re-encrypted under the same file key and nonce would hand the server two ciphertexts under one AES-GCM nonce. Two guards stand before any re-derived byte is sent:

- **Source identity** (best-effort): building the bundle records the source's size, modification time and file identifier, and every later read checks them first. A file system with coarse timestamps, or a writer that preserves them, can hide a change.
- **Chunk tags** (authoritative): the bundle's hashing pass records every STREAM chunk's authentication tag and persists the tags with the bundle, under the same key and nonce prefix, until every blob of the bundle is `Completed` or the bundle is abandoned. A re-created session reuses the key and nonce prefix, so the tags outlive any one session. A re-derived chunk whose tag differs is never sent. A range is sent only from whole STREAM chunks that have been re-derived and checked, even where it starts or ends inside one.

Either mismatch aborts the blob with a typed source-changed error before that read's bytes are sent. The finalize-time content-address check remains the backstop: a changed source can cost an upload, never store bytes under the wrong address.

**Ranged read** of plaintext byte `B`: `i = B / plaintext_chunk`, then fetch blob bytes at offset `56 + i × ciphertext_chunk` (chunk + 16-byte tag). Chunk `i` decrypts independently with the header as AAD, so the reader needs the header first: it rebuilds it from the manifest's mirrored fields or reads bytes `0..56` once and caches them with the asset. The formula is normative: using the plaintext stride as the ciphertext offset, or omitting the header offset, is a bug.

### Re-keying on Rewrite

`replace`, `derivative-replace` and `metadata-update` ([](/design/authorization/#the-closed-action-set)) keep the same id and may land in the same epoch. The fresh nonce folded into the KDF salt re-rolls **both** key and nonce, so identical plaintext yields different ciphertext and a different commitment.

- A prior `(key, nonce)` pair MUST NOT be reused.
- As defense in depth over the CSPRNG, the writer refuses a `nonce_prefix` or metadata nonce it has already emitted for that id.
- **The server detects a repeated prefix, without a key.** At finalize it refuses a manifest that repeats a prior manifest's `(asset_id, amk_version, nonce_prefix)` with a different ciphertext hash, logs it at `ERROR` and reports it to the writer. An identical hash is a deterministic re-seal and passes. This is detection and reporting, not containment: the server already holds both ciphertexts by then, so the refusal keeps the second out of the library but cannot undo the reuse. It catches a repeat across devices, which the writer's own refusal cannot ([](/design/threat-model/risks/)).

### STREAM Construction

AES-256-GCM under STREAM (Hoang–Reyhanitabar–Rogaway–Vizár 2015) via RustCrypto `aead::stream::{EncryptorBE32, DecryptorBE32}<Aes256Gcm>`, keyed by `aead_key`. The nonce is `prefix(7) || counter(4) || last_flag(1)`, which detects truncation, reordering and chunk deletion; the header as AAD binds every chunk to its blob's suite, epoch and commitment. A 65,520 B plaintext chunk becomes a 64 KiB ciphertext chunk, and an empty plaintext is one empty last chunk (a bare tag). The upload transport's chunk alignment is a separate concern: the 56-byte header offsets STREAM chunks from the upload grid, and the upload protocol counts blob bytes, not STREAM chunks.

## Metadata Encryption

| Data                                                                                       | Server view                                                    |
| ------------------------------------------------------------------------------------------ | -------------------------------------------------------------- |
| CBOR metadata blob (incl. `dominant_color` and the LQIP from [](/design/thumbnails/#lqip)) | Encrypted under the AMK; versioned and signed like a manifest  |
| `owner_id`, blob hash, blob size                                                           | Plaintext, because routing and quota need them                 |
| AI embeddings (search, faces)                                                              | Plaintext locally only; always encrypted at rest on the server |

The blob plaintext is the signed `SidecarV1` ([](/design/metadata/#sidecar-schema-v1)) in canonical CBOR ([](/design/metadata/#canonical-cbor-encoding)). Deterministic encoding is mandatory: the blob hash is signed, so encoder drift breaks cross-peer verification.

### Metadata Blob Wire Format

Implementations MUST produce and consume exactly:

```text
header (56, kind = 0x02, nonce = fresh CSPRNG) || AES-256-GCM(aead_key, nonce, plaintext, aad = header) || tag (16)
```

- The header's `suite_id` MUST equal the manifest's `crypto_suite_id` ([](/design/cryptography/primitives/#versioning-identifiers)) and is authenticated as AAD.
- Content key: `K = HKDF-SHA512(AMK_v{n}, salt = blob_id || nonce, info = "metadata-blob/v1", L = 32)`, and `aead_key` and the commitment follow from `K` ([](#key-commitment)).
- The content hash covers the whole byte string, header included, and is committed as `metadata_blob_hash` on every `create`, `replace` and `metadata-update`, beside the mirrored commitment, under both manifest signatures.
- **Local–server equivalence:** on decode the plaintext MUST equal the locally stored signed sidecar byte-for-byte and the hash MUST equal `metadata_blob_hash`, otherwise quarantine ([](/design/threat-model/validation/#client-side-validation-invariants)). See [](/design/metadata/#local-and-server-metadata-equivalence).
- Signed album and group documents and the share metadata record ([](/design/share-links/)) use this format with their own content keys; the share record carries `key_epoch = 0`.

## Key Envelopes

A key envelope (`nonce || ciphertext || tag` around a key) is a field of a signed or server-held record, never a content-addressed blob, so it carries no blob header: its record names the key that opens it. An envelope MUST NOT carry an unauthenticated selector. Where one carries a suite id or KDF parameters in-band, they are bound as AAD (or, for Argon2id parameters, bounded as in [](/design/cryptography/primitives/#password-based-kdf)).

## Failure Modes

| Failure                                                 | Detected / contained by                                                                                                                                                                                              | Recovery                                                                                                           |
| ------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Corruption, truncation or reorder at rest or in transit | Content address over header and ciphertext; STREAM tags                                                                                                                                                              | Re-fetch a content-addressed copy (path 6 of [](/design/backup-recovery/#recovery-paths))                          |
| Wrong, stale or forged AMK                              | Key commitment ([](#reading-a-blob), state 5)                                                                                                                                                                        | Re-obtain the key; re-key on a persistent divergence                                                               |
| Uploader sealed a blob that does not open               | Commitment matches, tag fails (state 6)                                                                                                                                                                              | Report to the uploader; `replace` from its plaintext                                                               |
| Unknown format or suite                                 | Header read order (state 2)                                                                                                                                                                                          | Upgrade; the blob is kept                                                                                          |
| Header tampered or swapped                              | Address; header as AAD; commitment salt                                                                                                                                                                              | As corruption                                                                                                      |
| Nonce reuse                                             | Prevented structurally (a fresh prefix per encryption); a repeat that slips through is detected and reported at finalize, not contained, since the server already holds both ciphertexts ([](#re-keying-on-rewrite)) | The writer re-seals the asset with a `replace` under a fresh prefix; the exposure is confined to that one file key |
| Sealed bytes that do not open                           | Open after seal, before signing ([](#encryption-workflow))                                                                                                                                                           | The write aborts; the source is kept                                                                               |

## Known Gaps

The header, commitment and classification are not implemented. Today blobs are headerless STREAM ciphertext (`capsule-core/src/crypto/encryption/stream.rs`) and `suite || nonce || ct || tag` metadata blobs with no AAD (`capsule-core/src/crypto/encryption/blob.rs`), and the fetch path reports any hash-correct blob that fails a tag as `FetchError::Corrupt`. The drop and share scope envelopes reuse the metadata-blob seal, so their suite prefix is not authenticated either. No writer opens what it sealed before signing, and the server has no repeated-prefix check.

## Validation

- Round-trip and fixed vectors for STREAM and single-shot blobs, header included, computed by an independent AES-GCM and HKDF implementation.
- STREAM tamper: bit flip, swap, drop and last-flag toggle are all rejected; so is every single-byte change to the header, on every chunk.
- A ranged read of chunk `i` at `56 + i × ciphertext_chunk` matches the plaintext slice; stitched ranges equal sequential decrypt.
- **Classification:** one fixture per state of [](#reading-a-blob), each mapped to its typed outcome, plus a ranged-read tag failure that stays unclassified until the whole-blob check.
- **Commitment:** a ciphertext crafted to open under two keys is rejected under the key whose commitment it does not carry; the commitment compare is constant-time.
- **Header and manifest agreement:** each disagreeing field is refused at finalize and classified _sealed bad at source_ by a client; a wrapped-mode asset with `key_epoch = 0` and a non-zero `amk_version` agrees, and one with `key_epoch = amk_version` does not.
- Metadata blob determinism against the shared canonical-CBOR vectors, a **blocking** cross-language gate.
- Rewrite re-roll: the same id and epoch yield a different key, nonce and commitment; a reused nonce is refused.
- Sidecar → blob → sidecar is byte-identical and hash-matches; a one-byte local mutation fails.
