---
title: Signed Manifests and Provenance
description: Capsule's signed asset manifest, append-only provenance chains, and derivative provenance
status: draft
---

Every stored asset carries a client-signed **manifest**; its manifests form an **append-only, hash-chained provenance log**. This separates legitimate writes from malicious or buggy ones after the fact and defeats [stale revival](/design/threat-model/scenarios/#damage-scenario--invariant-map).

Structs: `capsule-core/src/crypto/provenance/`. Verification: the `verify_asset` chokepoint ([Write Authorization](/design/cryptography/keys/#write-authorization)). Server-signed counterpart: [custody receipts](/design/transfer/durability/#custody-receipts).

## Asset Manifest

A signed manifest, not a Merkle tree: [STREAM](/design/cryptography/encryption/#stream-construction) already catches chunk tampering, truncation and reordering. Load-bearing fields:

| Field                                            | Rule                                                                                                                                                                                                  |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crypto_suite_id`, `protocol_version`            | Signed [suite](/design/cryptography/primitives/#versioning-identifiers) and album protocol pin; not downgradable.                                               |
| `file_id`, `album_id`, `amk_version`             | `amk_version` selects the epoch whose write-tier key verifies `write_sig`.                                                                                                              |
| `ciphertext_hash`                                | Content address of the original blob, hashed while streaming.                                                                                                                         |
| `metadata_blob_hash`                             | Content address of the [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption). Present on `create`, `replace`, `metadata-update` only. |
| `plaintext_size`, `chunk_size`, `nonce_prefix`   | STREAM parameters; `nonce_prefix` random per file.                                                                                                                                                 |
| `key_mode`, `wrapped_file_key`                   | `derived` (default) or `wrapped` (key sealed under the AMK). See below.                                                         |
| `created_by_user`, `created_by_device`           | The **signer of this record**; the creator is the `create` record's signer.                                                                                    |
| `client_version`                                 | [Exact build](#client-build-identification); audit-only.                                                                                                                          |
| `timestamp`                                      | Self-asserted; audit-only.                                                                                                                                           |
| `action`                                         | Closed set owned by [Authorization](/design/authorization/#the-closed-action-set).                                                                                                                    |
| `prior_provenance_hash`                          | SHA-256 of the previous manifest. Null **only** on `create`, else rejected by client and server.                                             |
| `retention_until`                                | Set only on `delete` ([retention](/design/organization/)).                                                                                                                  |
| `device_sig`, `write_sig`                        | Hybrid Ed25519 + ML-DSA-65 over all fields. **Both, and both halves, must verify.**                                                                          |

`device_sig` (uploading [DSK](/design/cryptography/keys/#device-keys), chained to the user IK via the [device directory](/design/cryptography/keys/#device-directory)) proves provenance; `write_sig` (epoch [write-tier key](/design/cryptography/keys/#album-master-keys-amks)) proves write authority at `amk_version`.

**Physical placement.** Never embedded in ciphertext. The manifest is the bundle's **envelope object**: server-visible canonical CBOR (no secrets) uploaded under the `provenance` [blob role](/design/transfer/upload/#what-gets-uploaded) and stored verbatim, which makes the index [rebuildable without PostgreSQL](/design/filesystem/server/#recovering-the-index-from-blobs-alone). The server also projects the fields it reads into PostgreSQL. Other blobs are referenced by hash only.

**The signed bytes are the served bytes.** No surface ([sync feed](/design/sync/#discovering-what-changed), index rebuild, [federation](/design/federation/)) may re-encode a manifest from the projection: it holds no signature, and signatures cover one exact encoding.

**Rewrite re-rolls keys** ([Re-keying on Rewrite](/design/cryptography/encryption/#re-keying-on-rewrite)). The signed `metadata_blob_hash` binds server metadata to the client's [sidecar](/design/metadata/#local-and-server-metadata-equivalence).

**Wrapped key mode** exists only for an adopted [web-upload drop](/design/web-upload/#why-adopt-in-place): set on the adopting `create`, never on `replace`. It is authorization-neutral ([Asset Key Derivation](/design/cryptography/encryption/#asset-key-derivation)).

**Wire presence rules (normative)**; present-`null` and absent keys sign differently:

- `prior_provenance_hash` and `retention_until` encode as **present `null`** when unset.
- `metadata_blob_hash`, `key_mode`, `wrapped_file_key`, `upgraded_from`, and the `DerivativeManifest`'s `amk_version` and `protocol_version` encode as **absent keys** when unset. Absent `key_mode` means `derived`; writing it explicitly breaks verification.

## Client Build Identification

`client_version` / `generated_by_client` pin a write to its exact build:

```text
client_version = client_id "/" semver "+" commit [".dirty"]
client_id      capsule-ios | capsule-android | capsule-web | capsule-cli | <out-of-repo client id>
commit         git hash of the client's own tree (full, or a prefix of at least 12 hex chars)
```

Example: `capsule-ios/1.4.2+9f3a1c7d2b4e`; nothing else (no OS or model). Audit-only: the server enforces only the [size bound](/design/threat-model/schema-rules/). Parser: `capsule-core/src/client_build.rs`.

## Provenance of Library Modifications

Every data or metadata change appends a record anchored by its [manifest](#asset-manifest).

### Chained, Append-Only Structure

`ProvenanceRecord { asset_id, manifest, prior_provenance_hash }`. The record's hash mirrors the manifest's (so the link is signed) and is a checked invariant: a record whose copies differ is **rejected**. Rewriting any past record breaks the forward walk from `create`.

Clients cache the verified head (O(1) steady state); full walks run on first fetch, restore, or suspected tamper. A signed checkpoint record is the designated v2 extension.

### What an Attacker With All Current Keys Still Cannot Do

Holding every current DSK, AMK and write-tier key, an attacker **can** append, but **cannot rewrite** a past record (that needs the earlier device's hybrid signature, verifiable via the [device directory](/design/cryptography/keys/#device-directory)) and **cannot remove** one (successors carry its hash). History is read-only.

### Legacy Inherited-Device Records

Older clients ([#475](https://github.com/Capsulsaurus/Capsule/issues/475)) copied `created_by_*` from the chain head into continuations while signing with the current device; such records from a second device of the account never verify, and exist in stored libraries.

**Quarantine in place, with a distinct signal.** No re-signing (rewrites history) and no accept-with-gap (trusts an unverifiable field). The chain stalls: later records fail as `ForgedChain`; the asset stays at its last verified state.

A `BadDeviceSig` becomes `LegacyInheritedDevice { named_device, signing_device, record_hash }` only when **all** hold (unreachable without an account device key):

1. the record is a continuation;
2. `device_sig` verifies under a **different** device of the same directory, added no later than the record's timestamp;
3. every other check passes (epoch attested, `write_sig`, chain link).

`record_hash` lets later `ForgedChain` verdicts be filed under the stall rather than as an attack. Reporting changes; acceptance does not. A shared-album member's write cannot meet (2) and stays `Rejected(BadDeviceSig)`. No migration.

### Physical Storage

- **Client:** append-only `media/{YYYY}/{YYYY-MM}/{uuid}.provenance.cbor`, kept as a tombstone after hard-delete. A **non-authoritative cache**, rebuilt from the server on mismatch.
- **Server:** the append-only envelope sequence, served unchanged. It reads `prior_provenance_hash`, `action`, `retention_until` to enforce chain advance and the purge floor keylessly ([Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants)).

No API overwrites or deletes an entry: the [action set](/design/authorization/#the-closed-action-set) has no such action and [invariant 17](/design/threat-model/validation/#in-the-index-critical-section-every-manifest-write-upload-or-lifecycle) refuses non-advancing writes.

## Derivative Provenance

Derivatives are signed so no client can silently overwrite another's. `DerivativeManifest` differs from `AssetManifest` in:

- `source_asset_id`;
- `role`, a closed set: `thumbnail | preview | embedding` (LQIP lives in the signed sidecar instead);
- `format`, closed; sentinel `original` means the tier is the original (no blob);
- `generated_by_*`, `generated_at`, `model_id`/`model_version` (embeddings);
- `amk_version`, `protocol_version`: **required on every real write** (absent only in legacy fixtures);
- `prior_provenance_hash`, chained per `(asset_id, role)`;
- `device_sig` and `write_sig`, under the same rules as the asset manifest.

It travels as the `derivative_manifest` blob, served verbatim; only it carries the `nonce_prefix` and `amk_version` that open the derivative. An overwrite is a `derivative-replace` on the role's chain. `verify_derivative` checks both signatures, `source_asset_id`, and the chain link; the receiver walks each role's chain from its start, and a refused or forked link stops it: prefix accepted, rest quarantined and surfaced, never applied.

## Validation

Negative cases map to [damage scenarios](/design/threat-model/scenarios/#damage-scenario--invariant-map).

- **`verify_asset`:** fixed positive vectors; one negative fixture each for reader-signed, retired epoch, wrong epoch, forged cert chain (unknown device or late `added_at`), replay, suite downgrade.
- **Wrapped key.** Tampering with `key_mode` or `wrapped_file_key` fails verification; a valid wrapped key unwraps and decrypts.
- **Chain advance.** A wrong `prior_provenance_hash` is rejected by both client and server.
- **Append-only.** Dropped or rewritten records break the walk; the server refuses overwrite/delete; a tampered cache is rebuilt.
- **Derivative poisoning.** A `derivative-replace` that does not chain to the `(asset_id, role)` head is rejected, and the existing derivative is kept.
- **Client build.** Producers emit the grammar with a real commit; arbitrary strings still verify.
- **Byte-identical serving (smoke).** Every serving surface returns uploaded bytes; a server re-encode fails the signature.
- **All current keys.** Rewriting a past record is detected.

The cross-module upload→verify path is listed under [Module Map — E2E](/design/module-map/#e2e-test-surface).
