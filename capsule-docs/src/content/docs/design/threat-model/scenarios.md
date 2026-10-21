---
title: Damage Scenarios and Quarantine
description: The damage-scenario → invariant map, quarantine surface inventory, and provenance immutability rules
status: draft
---

## Damage Scenario → Invariant Map

One vector per row; each defense has one owner doc.

| #   | Scenario                                                    | Defense                                                                                     | Owner |
| --- | ----------------------------------------------------------- | ------------------------------------------------------------------------------------------- | ----- |
| 1   | Old client strips unknown sidecar fields | Signature covers `_unknown`; newer schema never written | [Metadata](/design/metadata/#schema-versioning-rules) |
| 2   | Bytes mismatch declared content type | `content_type` allow-list + sandboxed decoder | [Validation](/design/threat-model/validation/#server-side-validation-invariants) |
| 3   | Chunk retried at wrong offset | Same offset, different `chunk_hash` → reject | [Upload](/design/transfer/upload/#chunk-rules-and-strictness) |
| 4   | Old manifest replayed to revive a delete | `prior_provenance_hash` advance check, both sides | [Provenance](/design/cryptography/provenance/#provenance-of-library-modifications) |
| 5   | Re-sign under a weaker suite | Signature covers suite and protocol version | [Keys](/design/cryptography/keys/#write-authorization) |
| 6   | Concurrent captions                                         | LWW + `superseded_captions` surfaces the loser                                              | [Metadata](/design/metadata/#surfacing-concurrent-edits) |
| 7   | OR-set remove of an unobserved add                          | Removes target a known `add_id`                                                             | [Metadata](/design/metadata/#add-id-binding) |
| 8   | Corrupt thumbnail overwrites a good one | Signed `DerivativeManifest` chain | [Provenance](/design/cryptography/provenance/#derivative-provenance) |
| 9   | Fake `timestamp` distorts audit | Audit-only; server `received_at` authoritative | [Keys](/design/cryptography/keys/#write-authorization) |
| 10  | Dedup TOCTOU duplicates a blob | Check + pending insert in one transaction | [Filesystem](/design/filesystem/server/#content-addressing-and-deduplication) |
| 11  | Upload exceeds declared size | Cumulative bound at every chunk | [Upload](/design/transfer/upload/#chunk-rules-and-strictness) |
| 12  | Unknown `crypto_suite_id`                                   | `400` at handshake, before any session                                                      | [Validation](/design/threat-model/validation/#protocol-and-capability-negotiation) |
| 13  | Peer floods the rejected-hash table                         | Per-peer quota, bounded LRU                                                                 | [Federation](/design/federation/#soft-fail-semantics) |
| 14  | Model swap invalidates AI tags | Model id+version per entry; no cross-model compare | [Metadata](/design/metadata/#tag-provenance-and-namespacing) |
| 15  | Leaked session revokes the rest                 | `revoke_all_sessions` needs master-key proof                                                | [Authentication](/design/authentication/#explicit-revocation) |
| 16  | Holder of all current keys rewrites history | Hash chain signed by past (retired) devices | [Provenance](/design/cryptography/provenance/#provenance-of-library-modifications) |
| 17  | Random `amk_version` skips MLS                              | Monotonic, server-known `amk_version`                                                       | [Validation](/design/threat-model/validation/#server-side-validation-invariants) |
| 18  | v_old client writes to an upgraded album | Stale `intent_id` → `409` | [Versioning](/design/versioning/#album-upgrade-ceremony) |
| 19  | Malformed sidecar after crash                               | Quarantined, never skipped                                                                  | [Repair](/design/filesystem/maintenance/#repair) |
| 20  | Manifest names an unlisted device                   | `created_by_device` must be in the directory                                                | [Validation](/design/threat-model/validation/#server-side-validation-invariants) |
| 21  | Hand-crafted metadata blob envelope | Byte-exact wire format, checked at decode | [Encryption](/design/cryptography/encryption/#metadata-blob-wire-format) |
| 22  | Retried delete double-decrements                 | Manifest idempotency on `prior_provenance_hash`                                             | [Validation](/design/threat-model/validation/#idempotency-invariants) |
| 23  | Old backup overwrites current state | Chain reconciliation; newer local wins; divergence quarantined | [Backup](/design/backup-recovery/#backup-verification) |
| 24  | Device predates its enrollment | IK-signed `added_at` must precede the manifest | [Keys](/design/cryptography/keys/#device-keys) |
| 25  | Peer floods hints | Advisory; pulls fully validated | [Federation](/design/federation/#pull-only-federation) |
| 26  | Peer TLS key silently changes | TOFU pin; rotation needs multi-vantage corroboration | [Federation](/design/federation/#server-identity-and-key-rotation) |
| 27  | Partial stack edit                                          | Bundle-atomic `.tmp` staging and rename                                                     | [Filesystem](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery) |
| 28  | Revoked capability still served                             | TTL ≤ 24h; revocations polled ≤ 15 min                                                 | [Federation](/design/federation/#federation-capabilities) |
| 29  | Embeddings from an unknown model                            | Vector index refuses unknown `model_id`                                                     | [AI](/design/ai/#embedding-provenance) |
| 30  | Client forges server-derived fields | Server recomputes; ignores client values | [Upload](/design/transfer/upload/#finalization-and-integrity) |
| 31  | Stale directory undoes a revocation | Master-signed monotonic `directory_version`; reader high-water mark | [Keys](/design/cryptography/keys/#device-directory) |
| 32  | Server fabricates or rewinds an epoch | Ceiling from signed MLS commits; in-flight key → *pending* | [Keys](/design/cryptography/keys/#write-authorization) |
| 33  | Share link enumerated | ≥128-bit id, rate limits, indistinguishable `404` | [Shares](/design/share-links/#security-contract) |
| 34  | Peer mass-reports a user | Signed, rate-limited; never server-wide | [Moderation](/design/moderation/#federated-reporting) |
| 35  | Federated pulls exhaust storage | Charged to receiver, capped per source peer | [Quota](/design/quota/#accounting-model) |
| 36  | Leaked upload link floods owner | Link caps, quota at creation, rate limits, revocation | [Drops](/design/web-upload/#security-contract) |
| 37  | Hostile file via upload link | Inbox only; sandboxed review before adoption | [Drops](/design/web-upload/#drop-and-adoption-lifecycle) |
| 38  | Server swaps the drop encapsulation key | Drop public key travels only in the URL fragment | [Drops](/design/web-upload/#two-confidentiality-properties) |
| 39  | Guest drop passed off as user-authored | No manifest; adoption mints an adopter-signed `create` | [Drops](/design/web-upload/#drop-and-adoption-lifecycle) |
| 40  | Upload-link passphrase brute force | Argon2id verifier, rate limits, caps | [Drops](/design/web-upload/#security-contract) |
| 41  | Doctored capability `min_protocol_version` | Signed claim; handshake and album pin still apply | [Federation](/design/federation/#federation-capabilities) |
| 42  | Offline share passphrase brute force | Argon2id per guess; rate-limited fetch; fragment secret | [Shares](/design/share-links/#security-contract) |
| 43  | Server loses bytes, denies accepting them | Signed `CustodyReceipt`; loss is provable | [Custody](/design/transfer/durability/#proof-of-loss) |
| 44  | Client claims custody it never had | No receipt, no claim | [Custody](/design/transfer/durability/#proof-of-loss) |
| 45  | Receipt replayed elsewhere / stale attestation | Server id signed in; attestations echo a client `nonce` | [Custody](/design/transfer/durability/#custody-receipts) |
| 46  | Wake flood drains battery | Contentless, coalesced; degrades to polling | [Notifications](/design/notifications/#4-registration-and-abuse) |
| 47  | Wake timing leaks activity | Contentless, coalesced, jittered; self-hostable transports | [Notifications](/design/notifications/#3-self-hostable-transports-only) |

## Quarantine Surfaces

Every "don't apply, surface it" path. A quarantined item is **never silently dropped or applied**; a human inspects, repairs, or discards it.

- [`verify_asset`](/design/cryptography/keys/#write-authorization) reject → quarantine area, audit log.
- [Federation soft-fail](/design/federation/#soft-fail-semantics) → bounded rejected-hash table.
- Orphaned original, malformed sidecar → `.library/quarantine/`, bytes preserved ([Repair](/design/filesystem/maintenance/#repair)).
- [Stale revival](/design/cryptography/provenance/#provenance-of-library-modifications) or [restore conflict](/design/backup-recovery/#backup-verification) → audit log + UI.
- Write stranded by an [album upgrade](/design/versioning/#album-upgrade-ceremony) → local `pending_until_upgrade` queue.
- [Drop awaiting adoption](/design/web-upload/#drop-and-adoption-lifecycle) → owner's server inbox.

## Provenance Immutability Rules

Policy over the [provenance chain](/design/cryptography/provenance/#provenance-of-library-modifications):

- No path (API, filesystem, federation) overwrites or deletes an entry; append-only by construction.
- Hard-delete removes ciphertext and metadata; the chain stays as a tombstone.
- Backups carry the full chain; restore [reconciles](/design/backup-recovery/#backup-verification).
- Current-key compromise can only append: past signers stay in the [device directory](/design/cryptography/keys/#device-directory) forever (`revoked_at`, never pruned).
