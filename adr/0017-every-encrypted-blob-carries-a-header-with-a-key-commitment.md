# ADR-0017 — Every encrypted blob carries a self-describing header with a key commitment

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** [ADR-0012](0012-originals-are-encrypted-in-bounded-memory.md), in part: its decision that originals keep the STREAM format unchanged. Its bounded-memory decision stands.
- **Superseded by:** —
- **Contract:** [Cryptography — Encryption § Blob Header](../capsule-docs/src/content/docs/design/cryptography/encryption.md#blob-header), [§ Reading a Blob](../capsule-docs/src/content/docs/design/cryptography/encryption.md#reading-a-blob), [Provenance § Asset Manifest](../capsule-docs/src/content/docs/design/cryptography/provenance.md#asset-manifest)
- **Issues:** #1209, #666

## Context

Two blob formats existed. Originals and derivatives were bare STREAM ciphertext: AES-256-GCM chunks with no prefix at all, whose nonce prefix, suite and epoch lived only in the signed manifest (`capsule-core/src/crypto/encryption/stream.rs`). Metadata blobs were `suite_id ‖ nonce ‖ ciphertext ‖ tag` with no associated data, so the suite id was checked only by comparing it with a manifest (`capsule-core/src/crypto/encryption/blob.rs`; #1209 item 7). ADR-0012 kept the STREAM format unchanged because bounded memory did not need a new one, and because the format freeze was near. Nothing has since been deployed, so no format is frozen.

The content address already detects corruption without any key: a fetch hashes what it received against the address it asked for and discards a mismatch (`FetchError::IntegrityFailed` in `capsule-sdk/src/fetch.rs`). Self-description is therefore not needed for corruption. What the old formats could not do was explain a blob that hashed correctly and still would not open. The fetch path reported every such blob as one `FetchError::Corrupt`, although it covers a wrong or stale AMK, a forged key delivery, and an uploader that sealed bad bytes, whose remedies are opposite: fetch a key, re-key the album, or ask the uploader to re-seal. AES-GCM is not key-committing, so a failed tag cannot tell these apart, and a ciphertext can be crafted that opens under two keys. A delegation courier, a backup verifier or a share viewer holding a blob without its manifest could not see its format, suite or epoch at all.

Narrower options were weighed: a header on metadata blobs only, a commitment in the manifest only, or no change. The manifest-only commitment leaves every manifest-less holder blind and still leaves the suite unauthenticated in-band. A metadata-only header keeps two formats and two read paths, which is the footgun the overhaul set out to remove.

## Decision

Every content-addressed encrypted blob, asset or metadata, begins with one fixed 56-byte header: magic, format version, construction kind, suite id, key epoch, nonce and a 32-byte key commitment. The AES key and the commitment are two HKDF-SHA512 expands of the blob's content key, salted with the header's first 24 bytes (`blob-aead/v1`, `blob-commit/v1`). The whole header is the associated data of every STREAM chunk and of the single-shot seal. The signed manifest mirrors each blob's commitment. The content address is SHA-256 over header and ciphertext together. A reader classifies a blob that will not open as corrupt, unsupported, sealed bad at source (a header that is malformed or disagrees with its manifest), key not held, wrong key, or sealed bad at source (a failed tag under the committed key), in that order, and each state has one client action.

## Consequences

- Every content address changes, and so do the known-answer vectors ADR-0012 pinned. With nothing deployed there is nothing to migrate.
- A blob grows by 56 bytes and costs one HMAC-SHA512 block more to seal or open. Quota, upload offsets and the ranged-read formula count the header: chunk `i` starts at `56 + i × 65,536`.
- The header is a deterministic function of the key and its fields, so an upload that re-derives ciphertext windows from the plaintext still reproduces the exact bytes, and ADR-0012's bounded-memory paths keep working with the header as the first window.
- A ranged reader needs the header before it can open any chunk. It rebuilds it from the manifest's mirrored fields or reads it once and caches it.
- `FetchError::Corrupt` splits into typed wrong-key and sealed-bad-at-source outcomes across the SDK, FFI and `--json` surfaces.
- The server gains a keyless check that each blob's header agrees with its manifest, so an inconsistent bundle is refused at finalize rather than discovered by a reader.
- Key envelopes (wrapped file key, drop and share scope wraps, master-key seal, password-wrapped secret) are not blobs and keep compact formats, but may no longer carry an unauthenticated selector.
