# ADR-0012 — Originals keep the STREAM format, and every path over them runs in bounded memory

- **Status:** accepted
- **Date:** 2026-10-22
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Cryptography — Encryption § STREAM Construction](../capsule-docs/src/content/docs/design/cryptography/encryption.md#stream-construction)
- **Issues:** #666

## Context

The server accepts originals up to 4 GiB. A push read the plaintext whole, re-encrypted it
into a `Vec<u8>` carried on the upload bundle, and handed that buffer to the upload client:
about twice the file's size in memory, on a device that may be a phone. A fetch assembled the
whole ciphertext in a `Vec<u8>` before checking its content address. The format freeze is the
last point at which the blob encryption format can change without a migration, so the question
was whether bounded memory needs a new, segmented AEAD format.

It does not. Blob encryption is already segmented: AES-256-GCM under STREAM, 65,520-byte
plaintext chunks, a nonce of `prefix(7) ‖ counter_be32(4) ‖ last_flag(1)`, and one tag per
64 KiB ciphertext chunk. Each chunk is authenticated on its own; the counter rejects
reordering and deletion; the last flag rejects truncation and extension; the per-encryption
prefix, folded into the file key, rejects a chunk lifted from another file. And because a
chunk's ciphertext depends only on its own plaintext, its index and whether it is last, any
byte range of a ciphertext can be re-derived from the plaintext without the rest of it. The
whole-file buffers were an implementation choice, not a property of the format.

## Decision

Originals and derivatives keep the STREAM format unchanged, and bounded memory is a property
of the code over it. A push reads each upload chunk as a window re-derived from the plaintext
on disk, so it holds one chunk window of an original, never the file. A fetch streams `Range`
windows into a writer, hashing as it goes, and a decrypting writer opens the ciphertext as it
arrives, holding at most two STREAM chunks. Building a bundle verifies the re-derived
ciphertext against the signed manifest in one streaming pass.

## Consequences

- No format change, so nothing to migrate, and every existing ciphertext, manifest and
  content address stays valid. Known-answer vectors computed by an independent AES-GCM
  implementation pin the format in the test suite.
- The upload bundle carries the original's per-file key so it can re-derive ciphertext
  windows lazily. It can open that one file and nothing else; the AMK still never leaves the
  workspace.
- A plaintext file that changes on disk after its bundle is built yields bytes the server's
  finalize-time content-address check refuses, or a read error if it shrank: never bytes
  stored under the wrong address.
- A streamed fetch writes unverified bytes until its content address checks out at the end,
  so a caller writing to a file discards it on any error.
- Paths that still need an original whole — the FFI's per-blob buffers, sync-apply's
  `original_ciphertext` slice, and the backup artifact — materialise it from the same source
  and are bounded-memory work of their own.
