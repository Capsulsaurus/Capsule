# Deferred Work

This file records what the **core data-plane implementation** (the `capsule-core`
cryptographic core + offline lifecycle + `capsule demo`) intentionally left for later, why,
and the seam that was left in place so it can drop in without reworking what exists. It
complements the design docs in `capsule-docs/src/content/docs/design/`.

## What is implemented and validated (offline, real crypto)

`capsule-core` implements and exhaustively unit-tests the full offline data plane:

- **Canonical CBOR** (RFC 8949 §4.2) — the byte-identity contract for every signature/hash.
- **Crypto primitives** — SHA-256 (streaming), HKDF-SHA512, Argon2id, AES-256-GCM (STREAM +
  standalone metadata-blob), **hybrid Ed25519 + ML-DSA-65** signatures (both halves required),
  **X-Wing (X25519 + ML-KEM-768)** hybrid DEK (known-answer-validated against
  `draft-connolly-cfrg-xwing-kem`).
- **Key hierarchy** — master key, default-album-id derivation, multi-epoch AMKs (offline
  rotation) + per-file/blob keys, software keystore (account ↔ encrypted `AccountFile`), signed
  device directory.
- **Encryption** — STREAM asset encryption with independent ranged-chunk decryption;
  exact metadata-blob wire format.
- **Provenance** — signed `AssetManifest`/`DerivativeManifest`, append-only hash-chained
  provenance, and the single **`verify_asset`** chokepoint (Accept / TerminalReject / Pending)
  with an exhaustive negative-case suite.
- **Validation invariants** — the key-less protocol handshake + structural envelope checks +
  idempotency keys.
- **CRDT metadata + Sidecar v1** — OR-set tags, LWW caption/rating with superseded log,
  monotonic add-id counter; signed `SidecarV1` (schema as CBOR field 0); privacy-on-export.
- **Backup** — deterministic signed tar artifact, AMK ledger, master-key escrow, Shamir
  2-of-3, and dry-run/commit restore with chain reconciliation.
- **Lifecycle `Workspace`** — ties it together and is showcased end-to-end by `capsule demo`.

## Deferred — with the seam in place

### Real MLS / OpenMLS group state
- **Why:** the design's MLS ciphersuite (`MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519`,
  `0x004D`) exists in `openmls` only via a C (`libcrux`) backend on a non-final IETF draft,
  with no IANA codepoint and no RustCrypto PQ backend yet (openmls#1940).
- **Seam:** `capsule_core::crypto::authority::AlbumAuthority` is the trait `verify_asset`
  consumes (epoch ceiling, per-epoch write-tier pubkey, AMK presence, admin-chain validity).
  `ReferenceAuthority` (an admin-signed epoch ledger) stands in for live MLS and is honored
  only via `&dyn AlbumAuthority`, so an `OpenMlsAuthority` drops in unchanged.
- **Now implemented offline:** **multi-epoch rotation** via `ReferenceAuthority` —
  `Workspace::rotate_epoch` mints AMK_v{n+1} + a fresh write-tier key and admin-attests the new
  epoch (assets imported before a rotation stay verifiable under their original epoch), plus a
  serializable, admin-signed `SignedEpochLedger` (`to_ledger`/`from_ledger`, whose admin chain is
  re-verified on reload; the local-only AMK-presence flag is restored out-of-band).
- **Still deferred with OpenMLS:** membership add/remove, the `Welcome`/history-delivery flow,
  and the album upgrade ceremony — these need live MLS group state, not just the epoch ledger.

### Hardware-bound key storage
- Device keys are kept in a **software keystore** (private keys sealed under the
  passphrase-wrapped master key). Secure Enclave / StrongBox / TPM adapters
  (`capsule-sdk::hardware-keys`) are per-platform glue, deferred.

### Networked server/client
- All transport is out of scope here: the HTTP/TUS upload server, GraphQL resolvers, the
  `/sync` feed, federation, peering, and the `capsule-sdk` network client. The **pure**
  refuse-by-default validation invariants those paths need are implemented in
  `capsule_core::validation` and ready to wire into `capsule-api`.
- The **adaptive cache-eviction policy** (bounded budget, LRU-by-last-access retention of
  recently-viewed blobs, tier-ordered eviction original → preview → thumbnail, pinned and
  device-owned originals exempt) is **now implemented** (issue #23): the
  `cached_representations` table + last-access tracking live in `capsule-core::db` and the sweep
  `capsule_core::library::cache_sweep` deletes evicted cache files (never the canonical `media/`
  files or the index). The byte budget is a plain parameter, so `capsule-sdk` connection-class
  detection drives it unchanged. Still deferred upstream: that connection-class budget detection
  and the wider networked server/client (HTTP/TUS, GraphQL, `/sync`, federation, peering).

### ML / AI
- Embeddings, `sqlite-vec` vector search, the model registry, semantic/face features, and
  moderation are deferred (explicitly out of scope). The sidecar reserves `tags_ai`
  (separate OR-set) and the manifest reserves `model_id`/`model_version` for them.

### Other
- Thumbnail/LQIP generation beyond `capsule-media`'s existing utilities.
- Fusing the crypto data plane into the **existing plaintext import executor**
  (`capsule_core::import::executor`): that pipeline still writes the legacy `AssetSidecar`.
  The crypto-integrated lifecycle lives in `capsule_core::lifecycle::Workspace` (used by
  `capsule demo`); unifying the two import paths is a follow-up.

## How to see it working

```
cargo test --workspace --exclude capsule-sdk      # full unit + e2e test surface
cargo run -p capsule-cli -- demo --workdir /tmp/capsule-demo   # narrated end-to-end showcase
```
