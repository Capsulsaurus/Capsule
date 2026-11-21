---
title: Damage Scenarios and Quarantine
description: The damage-scenario → invariant map, quarantine surface inventory, and provenance immutability rules
audit: none
---

## Damage Scenario → Invariant Map

One vector per row; each defense has one owner doc.

| # | Scenario | Defense | Owner |
| --- | --- | --- | --- |
| 1 | Old client strips unknown sidecar fields | Signature covers `_unknown`; newer schema never written | [](/design/metadata/#schema-versioning-rules) |
| 2 | Bytes mismatch declared content type | `content_type` allow-list; the decoder treats bytes as hostile whatever the type claims. The sandbox is post-v1, so v1 decodes in process ([](/design/clients/#sandboxed-decoder)) | [](/design/threat-model/validation/#server-side-validation-invariants) |
| 3 | Chunk retried at a conflicting range | Wholly committed → duplicate, nothing written; partial overlap → reject; changed bytes stopped by the client's chunk-tag guard | [](/design/transfer/upload/#chunk-rules-and-strictness) |
| 4 | Old manifest replayed to revive a delete | `prior_provenance_hash` advance check, both sides | [](/design/cryptography/provenance/#provenance-of-library-modifications) |
| 5 | Re-sign under a weaker suite | Signature covers suite and protocol version | [](/design/cryptography/keys/#write-authorization) |
| 6 | Concurrent captions | LWW by provenance-chain position, never `ts`; `superseded_captions` surfaces the loser | [](/design/metadata/#collaborative-metadata) |
| 7 | OR-set remove of an unobserved add | Removes target a known `add_id` | [](/design/metadata/#add-id-binding) |
| 8 | Corrupt thumbnail overwrites a good one | Signed `DerivativeManifest` chain | [](/design/cryptography/provenance/#derivative-provenance) |
| 9 | Fake `timestamp` distorts audit | Audit-only; server `received_at` authoritative | [](/design/cryptography/keys/#write-authorization) |
| 10 | Dedup TOCTOU duplicates a blob | Check + pending insert in one transaction | [](/design/filesystem/server/#content-addressing-and-deduplication) |
| 11 | Upload exceeds declared size | Range end checked against the declared size before writing | [](/design/transfer/upload/#chunk-rules-and-strictness) |
| 12 | Unknown `crypto_suite_id` | `400` at handshake, before any session | [](/design/threat-model/validation/#protocol-and-capability-negotiation) |
| 13 | Peer floods the rejected-hash table | Bounded LRU with a TTL. Eviction forgets only that a refusal happened: the refusal rests on the asset's superseded heads, and every refusal is logged and charged to the peer's error budget, which eviction does not touch | [](/design/federation/#soft-fail-semantics) |
| 14 | Model swap invalidates AI tags | Model id+version per entry; no cross-model compare | [](/design/metadata/#tag-provenance-and-namespacing) |
| 15 | Leaked session revokes the rest | `revoke_all_sessions` needs master-key proof | [](/design/authentication/#explicit-revocation) |
| 16 | Holder of all current keys rewrites history | Hash chain signed by past (retired) devices | [](/design/cryptography/provenance/#provenance-of-library-modifications) |
| 17 | Random `amk_version` skips MLS | Monotonic, server-known `amk_version` | [](/design/threat-model/validation/#server-side-validation-invariants) |
| 18 | v_old client writes to an upgraded album | Stale `intent_id` → `409` | [](/design/versioning/#album-upgrade-ceremony) |
| 19 | Malformed sidecar after crash | Quarantined, never skipped | [](/design/filesystem/maintenance/#repair) |
| 20 | Manifest names an unlisted device | `created_by_device` must be listed and unrevoked in the directory version in force at receipt (invariant 7) | [](/design/threat-model/validation/#server-side-validation-invariants) |
| 21 | Hand-crafted metadata blob envelope | Byte-exact wire format, checked at decode | [](/design/cryptography/encryption/#metadata-blob-wire-format) |
| 22 | Retried delete double-decrements | Manifest idempotency on `prior_provenance_hash` | [](/design/threat-model/validation/#idempotency-invariants) |
| 23 | Old backup overwrites current state | Chain reconciliation; newer local wins; divergence quarantined | [](/design/backup-recovery/#backup-verification) |
| 24 | Device signs before its enrollment or after its revocation, backdating `timestamp` | The device is checked against the directory version in force when the server receives the write; no self-asserted time is compared (invariant 7). Readers re-check against the directory version at the manifest's ledger position | [](/design/threat-model/validation/#server-side-validation-invariants) |
| 25 | Peer floods hints | Advisory; pulls fully validated | [](/design/federation/#pull-only-federation) |
| 26 | Peer TLS key silently changes | TOFU pin; rotation needs multi-vantage corroboration | [](/design/federation/#server-identity-and-key-rotation) |
| 27 | Partial stack edit | Bundle-atomic `.tmp` staging and rename | [](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery) |
| 28 | Revoked capability still served | TTL ≤ 24h; revocations polled ≤ 15 min | [](/design/federation/#federation-capabilities) |
| 29 | Embeddings from an unknown model | Vector index refuses unknown `model_id` | [](/design/ai/#embedding-provenance) |
| 30 | Client forges server-derived fields | Server recomputes; ignores client values | [](/design/transfer/upload/#finalization-and-integrity) |
| 31 | Stale directory undoes a revocation | Master-signed monotonic `directory_version`; reader high-water mark | [](/design/cryptography/keys/#device-directory) |
| 32 | Server fabricates or rewinds an epoch | Ceiling from signed MLS commits; in-flight key → *pending* | [](/design/cryptography/keys/#write-authorization) |
| 33 | Share link enumerated | ≥128-bit id, rate limits, indistinguishable `404` | [](/design/share-links/#security-contract) |
| 34 | Peer mass-reports a user | Signed, rate-limited; never server-wide | [](/design/moderation/#federated-reporting) |
| 35 | Federated pulls exhaust storage | Charged to receiver, capped per source peer | [](/design/quota/#accounting-model) |
| 36 | Leaked upload link floods owner | Link caps, quota at creation, rate limits, revocation | [](/design/web-upload/#security-contract) |
| 37 | Hostile file via upload link | Inbox only; owner review before adoption. The sandbox is post-v1, so v1 decodes drop bytes only with the memory-safe core codecs and adopts an undecodable drop without derivatives. A codec exploit remains the residual | [](/design/web-upload/#security-contract) |
| 38 | Server swaps the drop encapsulation key | Drop public key travels only in the URL fragment | [](/design/web-upload/#two-confidentiality-properties) |
| 39 | Guest drop passed off as user-authored | No manifest; adoption mints an adopter-signed `create` | [](/design/web-upload/#drop-and-adoption-lifecycle) |
| 40 | Upload-link passphrase brute force | Argon2id verifier, rate limits, caps | [](/design/web-upload/#security-contract) |
| 41 | Doctored capability `min_protocol_version` | Signed claim; handshake and album pin still apply | [](/design/federation/#federation-capabilities) |
| 42 | Offline share passphrase brute force | Argon2id per guess; rate-limited fetch; fragment secret | [](/design/share-links/#security-contract) |
| 43 | Server loses bytes, denies accepting them | Signed `CustodyReceipt`; loss is provable | [](/design/transfer/durability/#proof-of-loss) |
| 44 | Client claims custody it never had | No receipt, no claim | [](/design/transfer/durability/#proof-of-loss) |
| 45 | Receipt replayed elsewhere / stale attestation | Server id signed in; attestations echo a client `nonce` | [](/design/transfer/durability/#custody-receipts) |
| 46 | Wake flood drains battery | Contentless, coalesced; degrades to polling | [](/design/notifications/#4-registration-and-abuse) |
| 47 | Wake timing leaks activity | Contentless, coalesced, jittered; self-hostable transports | [](/design/notifications/#3-self-hostable-transports-only) |
| 48 | A member races a forged AMK delivery for an epoch | The AMK is exported from the epoch's MLS key schedule, never distributed; an `AlbumKeyDistribution` must equal the receiver's own export, the first valid delivery wins, and the key commitment turns a forged prior-epoch key into *wrong key* | [](/design/cryptography/keys/#key-delivery-and-who-may-send-it) |
| 49 | A ranged read meets a failed chunk tag | A ranged read cannot tell corruption at rest from a blob sealed bad, so it treats the window as corrupt and escalates to a whole-blob address check before it reports *sealed bad at source* | [](/design/cryptography/encryption/#reading-a-blob) |
| 50 | A stolen session overwrites the recovery escrow | Every escrow store is signed over the blob hash and a strictly higher generation: the master-key escrow by the recovery key or under the time lock, other slots by a directory device; a replacement nobody made is alerted at the next check | [](/design/backup-recovery/#master-key-escrow) |
| 51 | A stolen token enrolls its own TOTP factor and locks the owner out | TOTP enrollment needs a fresh credential and is refused over a confirmed factor | [](/design/authorization/#freshness-gated-operations) |
| 52 | A captured drop passphrase proof is replayed | The proof is an HMAC over the opaque id, the guest's timestamp and a fresh nonce: refused outside 5 minutes of the server clock or on a nonce seen in 10 minutes, and checked before any cap is reserved | [](/design/web-upload/#security-contract) |
| 53 | GC sweeps bytes that a fresh custody receipt names | The sweep's re-confirmation and unlink and a finalization's commit hold one per-address lock; live sessions and inbox drops are references | [](/design/filesystem/server/#deletion-and-garbage-collection) |
| 54 | A delegation bundle is replayed: to another device, after forfeiture, or after the asset's delete | The registration names the courier; a closed hand-off is `handoff_closed`; an already-uploaded bundle deduplicates; a create behind a later head is `stale_revival` | [](/design/peering/#threat-analysis) |
| 55 | A blob's header disagrees with its signed manifest | Finalization compares suite, epoch, nonce prefix and commitment without a key and refuses the bundle (invariant 36); a client classifies it *sealed bad at source* | [](/design/cryptography/encryption/#header-and-manifest-agreement) |
| 56 | A hostile server inflates a size or sends a decompression bomb | Every response body is capped per operation after decoding; a server-stated size is never trusted | [](/design/networking/#compression) |
| 57 | A password phisher pins its own identity key at registration | Registration carries the IK and its proof of possession, and the anchor is pinned in the act that creates the account (invariant 37) | [](/design/authentication/#the-identity-anchor) |
| 58 | A device thief replaces the recovery key under the IK and waits out the time lock | Every device announces the change and the current recovery key cancels it; a peer follows only a change the pinned recovery key signs, and an IK-only change raises a first-contact-grade warning | [](/design/cryptography/keys/#identity-rotation) |

## Quarantine Surfaces

Every "don't apply, surface it" path. A quarantined item is **never silently dropped or applied**: a human inspects, repairs, or discards it.

| Trigger | Held in |
| --- | --- |
| `verify_asset` reject ([](/design/cryptography/keys/#write-authorization)) | Quarantine area, audit log |
| Federation soft-fail ([](/design/federation/#soft-fail-semantics)) | Bounded rejected-hash table |
| Orphaned original, malformed sidecar | `.library/quarantine/`, bytes preserved ([](/design/filesystem/maintenance/#repair)) |
| Stale revival ([](/design/cryptography/provenance/#provenance-of-library-modifications)) or restore conflict ([](/design/backup-recovery/#backup-verification)) | Audit log + UI |
| Blob sealed bad at source, or a key divergence ([](/design/cryptography/encryption/#reading-a-blob)) | Asset marked unreadable, bytes kept, surfaced against `created_by_device` |
| Write stranded by an album upgrade ([](/design/versioning/#album-upgrade-ceremony)) | Local `pending_until_upgrade` queue |
| Drop awaiting adoption ([](/design/web-upload/#drop-and-adoption-lifecycle)) | Owner's server inbox |

## Provenance Immutability Rules

Policy over the provenance chain ([](/design/cryptography/provenance/#provenance-of-library-modifications)):

- No path (API, filesystem, federation) overwrites or deletes an entry: the chain is append-only by construction.
- Hard-delete removes ciphertext and metadata. The chain stays as a tombstone.
- Backups carry the full chain, and restore reconciles it ([](/design/backup-recovery/#backup-verification)).
- Current-key compromise can only append: past signers stay in the device directory ([](/design/cryptography/keys/#device-directory)) forever (`revoked_at`, never pruned).
