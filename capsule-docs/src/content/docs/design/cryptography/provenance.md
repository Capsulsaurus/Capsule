---
title: Signed Manifests and Provenance
description: Capsule's signed asset manifest, append-only provenance chains, and derivative provenance
---

Every asset Capsule stores has a verifiable trace of *who* produced it. The trace is anchored in a small **signed manifest** — bound to the ciphertext, cheap to verify, streaming-compatible — and extended by an **append-only, hash-chained provenance log per asset**. Together these are what let an operator distinguish a legitimate delete from a malicious or bug-induced one after the fact, and what defeats the [stale-revival attack](/design/threat-model/scenarios/#damage-scenario--invariant-map).

The schemas live here and are the **single source of truth** for `AssetManifest`, `ProvenanceRecord`, and `DerivativeManifest`. They are implemented in `capsule-core::crypto::provenance`; verification flows through the single `verify_asset` chokepoint in `capsule-core::crypto` ([Write Authorization](/design/cryptography/keys/#write-authorization)).

## Asset Manifest

A small signed manifest rather than a Merkle tree: the [STREAM construction](/design/cryptography/encryption/#stream-construction) already detects per-chunk tampering, truncation, and reordering, so a Merkle tree's only marginal gain (early-abort on a forged *whole-file* signature) is not worth the extra format complexity.

Each asset is stored as:

```rust
AssetManifest {
  version:                "asset-manifest/v1",
  crypto_suite_id:        u16,            // see Cryptography — Primitives
  protocol_version:       String,         // YYYY-MM-DD; matches album pin
  file_id:                UUID,
  album_id:               UUID,
  amk_version:            u32,            // identifies the AMK epoch + write-tier key
  ciphertext_hash:        bytes,          // content-address digest; algorithm fixed by crypto_suite_id; reused by upload protocol
  metadata_blob_hash:     bytes,          // content-address of the asset's encrypted metadata blob (see Encryption);
                                          //   a ciphertext hash (server-visible, no plaintext leak); set on
                                          //   create | replace | metadata-update
  plaintext_size:         u64,
  chunk_size:             u32,            // plaintext bytes per chunk (65,520)
  nonce_prefix:           [u8; 7],        // STREAM nonce prefix, random per file
  key_mode:               enum,           // derived | wrapped — how the file key is obtained
                                          //   derived (default): recomputed from the AMK; wrapped_file_key absent
                                          //   wrapped: carried in wrapped_file_key (an adopted web-upload drop)
  wrapped_file_key:       Option<bytes>,  // present iff key_mode = wrapped; the random file key sealed under the
                                          //   AMK (see Encryption — Asset Key Derivation). Opaque to the server.
  created_by_user:        UUID,
  created_by_device:      UUID,
  client_version:         String,
  timestamp:              RFC3339,        // self-asserted capture/write time; audit-only (see Keys — Write Authorization)
  action:                 enum,           // create | replace | delete | metadata-update
                                          //   | derivative-add | derivative-replace | trash-restore
  prior_provenance_hash:  Option<[u8;32]>, // SHA-256 over the previous manifest in this asset's
                                           // provenance chain. null only for `action = create`; a non-create manifest
                                           // with a null prior hash is rejected at verify_asset and by the
                                           // server's no-key chain-advance check (not a soft warning).
  retention_until:        Option<RFC3339>, // server-visible; set only for `action = delete` (see Organization — Retention Window)

  device_sig:        Hybrid(Ed25519, ML-DSA-65),  // over all fields above
  write_sig:         Hybrid(Ed25519, ML-DSA-65),  // under epoch write-tier key, over all fields above; both halves required
}

AssetBlob {
  manifest: AssetManifest,
  chunks:   [AES-256-GCM-STREAM encrypted chunks],
}
```

The manifest carries **two signatures**, and a client acknowledges the asset only if **both** verify:

1. `device_sig` — hybrid Ed25519 + ML-DSA-65 by the uploading device's [DSK](/design/cryptography/keys/#device-keys). Provides provenance; the device certificate chains to the user IK via the [device directory](/design/cryptography/keys/#device-directory).
2. `write_sig` — a **hybrid Ed25519 + ML-DSA-65** signature under the epoch's [write-tier key](/design/cryptography/keys/#album-master-keys-amks); both halves must verify. Proves the signer held write authorization at `amk_version` (see [Write Authorization](/design/cryptography/keys/#write-authorization)). The signature being hybrid is what keeps its coverage of `crypto_suite_id` non-downgradable even if one algorithm is later broken.

The signed manifest is stored as the encrypted asset's header and is itself part of the [provenance record](#provenance-of-library-modifications). The same signing approach applies to other surfaces — [metadata blobs](/design/cryptography/encryption/#metadata-encryption) and the [device directory](/design/cryptography/keys/#device-directory) are each hybrid, device-signed, and versioned.

**Streaming is preserved.** STREAM authentication tags verify every chunk *during* the stream. The manifest signature is a one-time provenance check. `ciphertext_hash` is computed incrementally as bytes arrive and confirmed at stream end — no separate pass, no buffering the whole file.

**Rewrite re-rolls keys and binds metadata.** A `replace` mints new ciphertext with a fresh `file_key` and `nonce_prefix` — re-rolled even under the same `file_id` and AMK epoch (see [Encryption — Re-keying on Rewrite](/design/cryptography/encryption/#re-keying-on-rewrite)). A `metadata-update` mints a new metadata blob the same way. Every `create`, `replace`, and `metadata-update` manifest commits to `metadata_blob_hash`, the content address of the asset's current encrypted metadata blob; because the field is covered by both signatures, the metadata bytes the server stores are signature-bound to the asset and cannot diverge from the [signed sidecar](/design/metadata/#local-and-server-metadata-equivalence) the client holds locally.

**Two ways the file key is delivered.** `key_mode` is a closed enum: `derived` (the default — the file key is recomputed from the AMK and `wrapped_file_key` is absent) or `wrapped` (the file key was chosen externally and is carried in `wrapped_file_key`, sealed under the AMK; see [Encryption — Asset Key Derivation](/design/cryptography/encryption/#asset-key-derivation)). Wrapped mode exists only for a [web-upload drop](/design/web-upload/) a client [adopts in place](/design/web-upload/#why-adopt-in-place); it is set at the adopting `create` and never on a `replace`. Both fields are covered by both signatures, so neither the mode nor the wrapped key can be altered without breaking verification, and the mode is [authorization-neutral](/design/cryptography/keys/#write-authorization) — it changes how a reader obtains the decryption key, never who was authorized to write. Like every manifest enum, `key_mode` is closed per [protocol version](/design/threat-model/schema-rules/).

The closed action enum is owned by [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set).

## Provenance of Library Modifications

Every modification of data or metadata produces a **provenance record** — timestamp, device, client version, action — anchored by the [signed manifest](#asset-manifest) above. The records form an **append-only, hash-chained log per asset**, which is the only structure that lets a key-holding attacker be detected after the fact.

### Chained, Append-Only Structure

```rust
ProvenanceRecord {
  asset_id:              UUID,
  manifest:              AssetManifest,           // see Asset Manifest above
  prior_provenance_hash: Option<[u8;32]>,         // SHA-256 over the previous record;
                                                  // null only for `action = create`
  // The manifest's own `prior_provenance_hash` mirrors this value, so signature
  // coverage of the manifest is signature coverage of the chain link itself.
}
```

Each non-create record references its predecessor by hash; a rewrite of any past record breaks the chain at that point and is detectable by any client walking forward from `create`.

### What an Attacker With All Current Keys Still Cannot Do

Even if every current key (every device's DSK, every album's current AMK and write-tier key) is compromised:

- **Forward writes are possible** — the attacker can append new records, just like any holder of those keys.
- **Past records cannot be rewritten** — the prior record was signed by a (possibly retired) device whose hybrid signature is still verifiable against the public half published in the [device directory](/design/cryptography/keys/#device-directory). Replacing the past record would require forging that earlier device's signature, which the hybrid construction prevents.
- **Past records cannot be silently removed** — every later record carries the prior hash, so a removal breaks the chain.

This bounds the blast radius of a credential compromise: history is read-only.

### Physical Storage

- **Client.** An append-only CBOR file at `media/{YYYY}/{YYYY-MM}/{uuid}.provenance.cbor`, alongside the asset and its sidecar — a sequence of `ProvenanceRecord` entries; on hard-delete the log persists as a tombstone-with-history. This file is a **non-authoritative local cache**. A faulty or malicious client can corrupt or truncate *its own* copy, but cannot rewrite history: the chain is self-authenticating — each record is signed and carries the prior record's hash, so dropping or altering any record breaks the forward walk from `create` — and the authoritative copy is the server's append-only blob sequence plus the replicas every other album member holds, any of which re-detects the tamper on next sync as a chain-head mismatch. A client that finds its local cache inconsistent with the authoritative chain rebuilds it from the server.
- **Server.** A content-addressed encrypted blob, distinct from the [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption), so a metadata edit (which mints a new metadata blob) never rewrites history. The server's no-key envelope of every provenance write includes `prior_provenance_hash`, so the server can enforce monotonic chain advance without holding any key — see [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants).

The server is **append-only** for provenance: there is no API path that overwrites or deletes an existing entry. An attempt is rejected at the [server's structural validation layer](/design/threat-model/validation/).

## Derivative Provenance

Thumbnails, previews, and embeddings are generated client-side and uploaded as ordinary encrypted blobs. Without provenance they would be silently overwritable by any client with write capability — a buggy v4 client could quietly replace a v3 client's good thumbnail with a corrupt one. To prevent this, every derivative carries a small signed manifest of its own:

```rust
DerivativeManifest {
  version:               "derivative-manifest/v1",
  crypto_suite_id:       u16,
  source_asset_id:       UUID,
  role:                  enum,            // thumbnail | preview | embedding (LQIP lives in the signed sidecar, not here)
  format:                String,          // e.g. "image/avif", "embedding/mobileclip-b"
  ciphertext_hash:       bytes,
  generated_by_device:   UUID,
  generated_by_client:   String,
  model_id:              Option<String>,  // for embeddings; see AI/ML Integrations
  model_version:         Option<String>,  // for embeddings
  generated_at:          RFC3339,
  prior_provenance_hash: Option<[u8;32]>, // chained per (asset_id, role)
  device_sig:            Hybrid(Ed25519, ML-DSA-65),
  write_sig:             Hybrid(Ed25519, ML-DSA-65),  // under the album's epoch write-tier key; both halves required
}
```

A derivative overwrite is therefore a `derivative-replace` lifecycle action that appends to the provenance chain like any other write. Quarantine semantics from [Write Authorization](/design/cryptography/keys/#write-authorization) apply: a derivative whose manifest fails verification is surfaced, never silently applied — a buggy client cannot poison a derivative under the receiving side's nose.

## Validation

This is the cryptography sub-doc most directly responsible for the `verify_asset` chokepoint that every consumer module depends on. Its unit-test surface must be exhaustive — every negative case is a real damage scenario from [Threat Model — § Damage Scenarios](/design/threat-model/scenarios/#damage-scenario--invariant-map).

- **`verify_asset` positive cases** — a manifest signed by the correct device + correct epoch write-tier key, with a matching `prior_provenance_hash`, verifies. Tested with fixed test vectors so a refactor cannot silently shift the contract.
- **`verify_asset` negative cases (exhaustive)** — reader-signed (no write-tier sig), removed-writer (write-tier sig from a now-retired epoch), wrong-epoch (sig from the wrong AMK version), forged certificate chain (device not in the user's directory or `added_at` postdates the manifest), replayed manifest (`prior_provenance_hash` does not match local chain head), suite-downgrade (re-signed under a weaker `crypto_suite_id`). Each case is its own unit test with a hand-crafted manifest fixture.
- **Wrapped-key mode (unit).** A `key_mode = wrapped` manifest whose `wrapped_file_key` (or `key_mode` itself) has been altered after signing fails `verify_asset` like any other tampered signed field; a member holding the AMK unwraps a valid `wrapped_file_key` to recover the file key and STREAM-decrypts the unchanged ciphertext. Exercises the [adopted web-upload drop](/design/web-upload/#why-adopt-in-place) path; authorization checks are unchanged from the derived case.
- **Chain advance enforcement** — unit test that appending a record whose `prior_provenance_hash` does not match the current head is rejected. Both client-side (`verify_asset`) and server-side (no-key envelope check) reject the same way.
- **Append-only enforcement (cryptographic, not just storage).** The guarantee is the signature chain, not the file mode. A unit test drops or rewrites a record in a serialized chain and asserts the forward walk from `create` detects the break (a non-matching prior hash, or a signature that no longer verifies). A companion test confirms the server rejects any overwrite or delete of an existing provenance entry at its structural validation layer (invariant 17), and that a client whose local `.provenance.cbor` has been tampered re-derives the authoritative chain from the server rather than trusting the local bytes.
- **Derivative poisoning rejection** — unit test that a `derivative-replace` whose `prior_provenance_hash` does not chain to the current head for `(asset_id, role)` is rejected; the existing derivative is preserved.
- **What-an-attacker-with-all-current-keys-still-cannot-do** — scenario test that holds every *current* key, attempts to rewrite a past record, and confirms the chain walker detects the break.

The cross-module case (a manifest moving through upload → server envelope validation → finalization → client `verify_asset` on download) is bounded E2E surface, listed in [Module Map](/design/module-map/#e2e-test-surface).
