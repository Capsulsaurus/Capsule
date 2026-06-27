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
- **Lifecycle `Workspace`** — ties it together and writes through to the queryable
  `library.sqlite` index; showcased end-to-end by `capsule demo`.

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

- **Seam + software fallback + contract — now implemented.** The device signing key (DSK) is
  consumed through a `capsule_core::crypto::keys::Signer` trait, so the in-memory
  `HybridSigningKey` (software, default) and a hardware-backed key are interchangeable at every
  signing site. `HardwareSigner` is a uniffi **foreign trait** (Secure Enclave / StrongBox / TPM
  implement it natively under the `ffi` feature); `HardwareBackedSigner` composes its
  hardware-produced **Ed25519** half with a software-sealed **ML-DSA-65** half into the hybrid
  signature (no secure element holds PQ keys). `Workspace::create_with_hardware_signer` /
  `FfiWorkspace.createWithHardwareSigner` build a workspace whose directory + manifests are
  hardware-signed; the in-process round-trip + non-exportability contract (`keys.md` Validation)
  runs in CI against a mock element.
- **Reference adapters + standalone harnesses — now implemented.** Every backend implements the
  `HardwareSigner` contract as a runnable, locally-testable example (the prose `HARDWARE_KEYS.md`
  guide is gone — the code is the example): a software fallback
  (`capsule_core::crypto::keys::SoftwareSigner`, smoke-tested in CI on Linux), a desktop TPM 2.0
  reference (`crypto::keys::tpm`, behind the `tpm` feature, via `tss-esapi`), and Secure Enclave /
  StrongBox adapters in standalone harness packages (`capsule-core-swift`, `capsule-core-kotlin`)
  that link the compiled core and run a per-language smoke test (see each package's README; the
  Swift `swift test` runs the real Secure Enclave on Apple-Silicon Macs).
- **Still deferred (per-platform glue):** the three hardware backends (Secure Enclave, StrongBox,
  TPM) all expose ECDSA-P256, not Ed25519, so they need the **P-256 hybrid-DSK variant** before
  they compose into the device key — only the software backend integrates end-to-end today; wiring
  the generated bindings + `cdylib`/`staticlib` into the real Xcode / Gradle apps; on-device CI;
  the Windows TPM (TBS) path; and hardware binding of the device **encryption** key (DEK).

### Networked server/client

- All transport is out of scope here: the HTTP/TUS upload server, GraphQL resolvers, the
  `/sync` feed, federation, peering, and the `capsule-sdk` network client. The **pure**
  refuse-by-default validation invariants those paths need are implemented in
  `capsule_core::validation` and ready to wire into `capsule-api`.
- The **storage-verification endpoint (`POST /storage/verify`) + verify-before-destroy gate**
  is deferred with the rest of the networked surface. The endpoint returns a key-free
  durability verdict (stored = blob-store `stat`; indexed = committed `uploaded=true` row;
  retrievable = refcount > 0 and not `collectable_since`/quarantined/dangling), and the
  standing client rule is that no destructive local cleanup of irreplaceable bytes proceeds
  without a `durable` verdict. **Seam:** the verdict is pure index+filesystem inspection that
  drops into `capsule-api-media`; the verify-before-destroy predicate is a pure `capsule-core`
  function the cache-eviction sweep, move-import, and streaming import all consume; the
  offline core's `verify_asset` already covers the complementary crypto-validity half of the
  gate.
- **Import-upload streaming mode + free-space probe** is deferred with the networked path. For
  storage-constrained devices, import runs a sliding-window import→upload→verify→release loop
  that bounds peak local disk to the in-flight window, auto-detected from a free-space probe and
  halting (no further source admitted into the library) if the server connection drops. **Seam:**
  the planner already emits `total_size`; what's added is `capsule_core::library::available_bytes()`
  (a thin `statvfs`/`GetDiskFreeSpaceEx` wrapper), the `streaming_recommended` plan flag, and an
  executor drive mode that interleaves the existing offline import, the deferred upload client, and
  the storage-verify gate above — with no change to the upload wire protocol.
- **Web upload (guest drops)** is deferred with the rest of the networked surface. A browser/WASM
  guest, holding only a provisioned **upload link**, seals each asset under a fresh random key
  encapsulated to a link-scoped **Drop Key** and uploads it to a per-user staging inbox charged to
  the provisioning user's quota; the bytes become a library asset only when one of that user's
  trusted clients reviews and **adopts** the drop, rewrapping the guest's key under the album AMK
  (`key_mode = wrapped` + `wrapped_file_key`) **without re-uploading**. Designed in the
  `web-upload` design doc. **Seam:** every primitive already exists offline — X-Wing
  encapsulation, AES-256-GCM-STREAM, content-addressing — so web upload is purely additive: a
  `capsule_core::drop` module (sealing + link issuance + adoption rewrap), a `capsule-api-media`
  drop store/inbox + the atomic inbox→album promotion, a drop upload protocol that reuses the
  existing upload-protocol chunk/finalization mechanics, and the `capsule-web` browser client. The
  offline core's `verify_asset`, append-only provenance chain, STREAM construction, and the
  sandboxed-decoder rule are reused unchanged; the only crypto-core addition is the **wrapped
  file-key mode** — a stored, AMK-wrapped per-file key for externally-authored bytes that cannot be
  re-derived from the AMK — landing in `crypto::encryption` key derivation (the `asset-keywrap/v1`
  label) and the `crypto::provenance` `AssetManifest` (`key_mode`/`wrapped_file_key`), with
  authorization unchanged (the adopter's write-tier signature remains the sole authority).
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

- **Rewrite re-keying + metadata↔manifest binding (contract tightened).** The
  offline crypto core already draws a fresh nonce per asset/metadata encryption
  and seals every metadata blob fresh. The design contract now additionally
  requires: (a) folding that fresh nonce into the key-derivation salt
  (`salt = file_id || nonce_prefix` for assets, `blob_id || nonce` for metadata)
  so a same-`file_id`/same-epoch `replace` and a constant-`blob_id`
  `metadata-update` re-roll the **key** as well as the nonce — making any
  `(key, nonce)` reuse structurally impossible; and (b) a new server-visible
  `metadata_blob_hash` field on the signed `AssetManifest` plus a client
  round-trip-equivalence check (decrypt of the metadata blob must equal the
  signed local sidecar) and the matching no-key server check (validation
  invariant 25). **Seam:** the key-salt change is local to `crypto::encryption`
  key derivation; the manifest field, the round-trip check in `verify_asset`, and
  the envelope hash-match check land in `crypto::provenance` /
  `crypto::verify_asset` and the `capsule-api` envelope validator when the
  networked write path is wired.
- Thumbnail/LQIP generation beyond `capsule-media`'s existing utilities.
- Fusing the crypto data plane into the **existing plaintext import executor**
  (`capsule_core::import::executor`) — **partially done.** `capsule_core::lifecycle::Workspace`
  now writes through to the shared `library.sqlite` index: every import / metadata edit /
  soft-delete upserts the queryable `assets` row (+ user tags) and records a device-owned
  `original` cache representation, so crypto-imported assets are timeline-queryable and feed the
  Phase-3 cache sweep. Dedup against `assets` is consequently **global** across both import
  paths. **Still deferred:** the legacy `import::executor` keeps writing the unsigned
  `AssetSidecar`; replacing it with the signed `SidecarV1` + manifest + provenance path needs
  the deferred thumbnail/LQIP (media) generation, so the full executor rewrite is a follow-up.

## How to see it working

```bash
cargo test --workspace --exclude capsule-sdk      # full unit + e2e test surface
cargo run -p capsule-cli -- demo --workdir /tmp/capsule-demo   # narrated end-to-end showcase
```
