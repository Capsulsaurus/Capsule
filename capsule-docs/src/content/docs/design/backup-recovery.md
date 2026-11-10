---
title: Backup and Recovery
description: The portable backup artifact, the master-key escrow, and the recovery flows
status: draft
---

**Rule:** holding the recovery secret restores every asset, even after every device is lost.

Two separate things are called "backup":

- The **backup artifact** ([](#backup-artifact)): a portable, encrypted export of a library's assets.
- The **master-key escrow** ([](#master-key-escrow)): a small server-side blob a passphrase unwraps into the key hierarchy.

A client MAY drop its only local copy only after the durability endpoint ([](/design/transfer/durability/)) confirms the server durably holds it.

## Backup Artifact

A self-describing, versioned, streamable archive, encrypted and independent of device keys and MLS ratchet state. Its format is a cross-platform contract: an artifact exported anywhere MUST restore everywhere.

### Container Format

Uncompressed POSIX tar:

```text
backup.tar
├── VERSION                    # plaintext: format version, crypto_suite_id, min_protocol_version
├── MANIFEST.cbor              # entry list, hashes, sizes, exporter identity; HMAC + hybrid signature
├── keys/amk-ledger.cbor       # every needed AMK version, wrapped under the backup wrap key
├── library/attestation-keys.cbor # pinned server attestation-key history, verbatim (optional)
└── <sorted by (album_id, asset_id, blob_role)>
    ├── blobs/{hash}           # ciphertext blobs
    ├── meta/{blob_id}         # encrypted metadata blobs
    ├── create-meta/{asset_id} # encrypted metadata blob the create binds, once an edit replaced it (optional)
    ├── provenance/{asset_id}  # full per-asset provenance chains
    ├── authority/{album_id}   # admin verifying key + admin-signed epoch ledger (format 2+)
    └── docs/{album|stack}/{id} # album/stack label record, sealed under its album's AMK (optional)
```

- **Self-sufficient.** The AMK ledger ([](/design/cryptography/keys/#album-master-keys-amks)) is wrapped under the passphrase-derived backup wrap key, not a device key, so the passphrase alone decrypts every included blob with no server or MLS state.
- **Uncompressed** (ciphertext is incompressible) and **streamable**: no central directory, so terabyte exports need no seeking. This is why it is not ZIP.
- **Deterministic.** Sorted entries and normalized ustar headers (`mtime`/`uid`/`gid` = 0, empty names, fixed modes, no PAX), so re-exports of the same content are byte-identical.
- **Manifest first.** `MANIFEST.cbor` follows `VERSION` and precedes every blob, so a streaming reader has the integrity list up front. Both checks MUST pass before restore:
  - **HMAC** under the wrap key ([](/design/cryptography/primitives/#password-based-kdf)) catches truncation, reordering and corruption before decrypt.
  - **Hybrid signature** ([](/design/cryptography/primitives/#signature-scheme)) by the exporter's DSK ([](/design/cryptography/keys/#device-keys)) defeats a wrap-key thief who re-HMACs. The exporter MUST be in the device directory ([](/design/cryptography/keys/#device-directory)) at restore; a revoked exporter is rejected.
- **Versioned** per [](/design/cryptography/primitives/#versioning-identifiers): old artifacts stay restorable, and an unknown `crypto_suite_id` is rejected.
- **`authority/` is public, not a capability.** It makes restored assets *verifiable* and carries no write-tier or admin seed, so an album new to the library restores read-only. Restore re-verifies the ledger's admin signature chain, adopts it only for an album with no authority of its own, and never replaces existing authority. Format-1 artifacts restore readable but unverifiable.

The optional entries below are additive within format 2: an older reader verifies and skips them.

- **`docs/` carries label records sealed.** Each exported album's `AlbumDocV1` (its name and labels; [](/design/organization/#labels)) and each labelled stack's `StackDocV1` stay sealed under the album's AMK, whose epoch the ledger escrows. Restore merges them, so a recovered album keeps its name.
- **`create-meta/` carries the create's metadata blob sealed.** An upload carries an asset's `create`, which only the exact blob its `metadata_blob_hash` names answers, and the first edit replaces that blob in `meta/`. An asset whose create binds a blob other than its head's therefore carries that blob here. Restore keeps it beside the chain before writing the chain, so an asset edited and never uploaded can still upload its create on the restoring device. A carried blob that does not answer the create's signed hash is dropped with a warning, never a reason to refuse the asset. An artifact without the entry restores as before.
- **`library/attestation-keys.cbor` carries key pins verbatim**: the library's pinned attestation-key history for each server ([](/design/transfer/durability/#custody-receipts)). A restored library keeps checking receipts under the pinned keys instead of trusting the server's next publication on first use. Restore keeps the union per server only where one history extends the other; histories that each hold a key the other lacks refuse the restore before anything is written. A pin whose schema is newer than the restoring build reads is skipped with a warning, never a reason to refuse the assets.

## Master-Key Escrow

The account master key ([](/design/cryptography/keys/)) is escrowed server-side, wrapped by a key derived from the recovery secret via the password-based KDF ([](/design/cryptography/primitives/#password-based-kdf)).

- The secret MUST carry **≥128 bits** (random code or BIP39-style phrase): the blob is offline-attackable and Argon2id adds only linear cost.
- A shorter PIN is permitted **only** behind an attested enclave that rate-limits unwraps (Signal SVR pattern). Never a short numeric code otherwise.
- One active escrow per account; replacing it deletes the old blob.

The CLI is `capsule auth escrow` and `capsule auth recover`. Setup:

- binds the library to the signed-in account, and refuses a library that already holds assets (its manifests name its own id);
- shows a fresh 256-bit secret once, behind a type-back gate;
- refuses to replace an existing escrow without `--replace`;
- once the library is bound, makes its device a member of the account's published device directory (publishing the library's own when the account has none), as recovery does for a recovered device, so the server accepts what the device uploads.

Every refusal writes nothing.

### Album Key Escrow

The master key and the sealed identity ([](/design/cryptography/keys/#registered-accounts)) open a recovered library as the account, but AMKs ([](/design/cryptography/keys/#album-master-keys-amks)) are random per album and epoch. Each album's key state is therefore escrowed beside the master-key escrow, so a device recovered from the recovery secret alone can open every album.

- **What is escrowed.** One blob per album: every epoch's AMK, the album's write-tier and admin seeds, and its authority, prefixed with a domain separator and sealed under the master key. It adds no root ([](#single-root-invariant)). A recovered device holds the album as the device that escrowed it did, so it can still write to it.
- **Where.** One opaque slot per album per account, stored or replaced individually in one operation and listed together ([](/reference/api/auth/)). Slots are never versioned and never deleted. Each slot is capped at 512 KiB, and an account at 4096 slots and 32 MiB in total. A store past either bound is refused with `error.escrow.album_keys_full` and writes nothing, which also bounds the listing a push reads.
- **Kept current before any asset leaves.** Setup escrows every album. Every push first lists the escrows, then stores each album the server lacks or that this device strictly extends (a new album or a new epoch). It does so before any asset is sent, and also when there is nothing to push. A refused escrow fails the push. The server must never hold an asset whose album key it does not also hold.
- **Never shrunk.** An escrow is replaced only by one holding every epoch it holds, under the same keys. If another device rotated further, the escrow holds epochs this device lacks, and this device's push leaves it alone; its own epochs are already covered.
- **Adopted before any album is resolved.** `capsule auth recover` fetches the escrows before deriving anything and, once the library is created, adopts every escrowed album before anything calls `ensure_album`. That ordering stops a recovered library minting a fresh AMK under the derived default-album id, which the account's other devices already wrote under.
- **Failure handling.** An escrow that opens but is inconsistent (served under another album's id, not an album escrow, or with an authority that does not re-verify) refuses the recovery, and the new library is removed. An escrow that does not open under the recovered master key is skipped: another library sealed it before the escrow was replaced, its albums belong to that library's key, and refusing them would lock the account out for good.
- **Known limit.** Two devices can each hold an album and rotate it independently, so each holds an epoch the other lacks, or the same epoch under another key. Neither may replace the other's escrow: it stays as the first writer left it, and the album is uncovered on the other device. That device's push holds its assets in the album back (`album_keys_not_escrowed`) rather than sending them unrecoverable. The same applies to an escrow that does not open under this device's master key and to an account whose escrows are full. Reconciling diverging epochs waits on the MLS membership work.

## Single-Root Invariant

**Every recovery path terminates at the one account master key, and the recovery secret (or a quorum of its Shamir shares) is the only user-held secret class. No feature may add a second escrowed secret class or a path that bypasses the master key.**

The escrow, the artifact wrap key and Shamir shares all derive from the same secret. OGK and Drop-Key escrows, the album key escrows ([](#album-key-escrow)) and the sealed user identity ([](/design/cryptography/keys/#registered-accounts)) are wraps *reachable from* the master key, not roots. Sponsored accounts ([](/design/authentication/#account-types)) hold no root; they route through the sponsor's, so they are never prompted to verify.

An exported artifact stays bound to the secret in force at export, so rotating the secret ends with "re-export or destroy old artifacts" guidance.

## Recovery Paths

Independent paths; every failure mode recovers through at least one, and each row of a page's
Failure Modes table is tested for its detection and its recovery:

| # | Path | Survives |
| --- | --- | --- |
| 1 | [](#master-key-escrow): passphrase/seed → master key → AMK escrow | Total device loss |
| 2 | Cross-device recovery ([](/design/device-enrollment/)) | Partial device loss; lost master-key backup if one device survives |
| 3 | [](/design/cryptography/keys/#owner-group-keys-ogks): any owner recovers every album's AMKs | Lost membership, AMK distribution gaps |
| 4 | Offline backup artifact ([](#backup-artifact)) | Server loss, account compromise, escrow corruption |
| 5 | Filesystem rebuild: CBOR sidecars are canonical, the DB is a cache (idempotent `rebuild_index()`) | DB corruption, index bugs |
| 6 | Content-addressed copies on devices or federated peers ([](/design/federation/); ciphertext, not keys) | Single-server data loss |
| 7 | Trash ([](/design/organization/#recycling)): signed soft `delete` with a retention window before purge | Erroneous deletes |

**Account types.** Registered accounts have all seven. Delegated/sponsored accounts ([](/design/authentication/#account-types)) recover only through the sponsor's master key ([](/design/cryptography/keys/#delegatedsponsored-accounts)). Non-registered share-link accounts ([](/design/share-links/)) have no collection to recover.

Per-topic failure modes, each naming the path it recovers by: [](/design/cryptography/keys/#failure-modes), [](/design/cryptography/encryption/#failure-modes), [](/design/cryptography/provenance/#failure-modes), [](/design/mls-resilience/#failure-modes), [](/design/metadata/#failure-modes) and [](#failure-modes) below.

## Recovery Mechanisms

The user-invoked mechanisms among the recovery paths ([](#recovery-paths)).

### Default Mechanisms

- **Recovery secret**, shown at setup, unwraps the escrow.
- **Cross-device recovery**: a surviving signed-in device re-bootstraps a new one over a verified channel, a ceremony shared with cross-device add ([](/design/device-enrollment/#cross-device-add)).

### Opt-in: Shamir Secret Sharing

- The recovery seed is split **2-of-3** by default (custom `m`-of-`n` allowed); fewer than `m` shares reveal nothing.
- Each share is additionally wrapped under its own passphrase via the password-based KDF.
- Reconstruction is client-side only; the server never sees more than one share or the seed.

#### Share Wire Format

A share is `x ‖ y`: one nonzero byte `x`, then one GF(256) byte per seed byte (33 bytes for a 32-byte seed), byte-wise Shamir over polynomial `0x11d` with uniformly random coefficients from a CSPRNG. Malformed, zero-`x` or duplicate-`x` shares are a typed error, never a panic. Shares issued by the retired `sharks` crate (RUSTSEC-2024-0398, ~0.18 bits leaked per share) use the same encoding and stay recoverable; guided re-wrap ([](#on-repeated-failure-guided-re-wrap)) re-issues them.

## Recovery Verification Cadence

Periodically the user proves they still hold the secret. The check MUST NOT become a lock-out or a guessing oracle.

### Local Verification

- **Local-only.** The client unwraps a cached copy of the escrow blob and compares an HKDF-derived tag against one derived from the device-held master key, never raw key bytes. No server round-trip; a failure locks nothing.
- **Stale cache.** Before recording a failure, refresh the blob and retry once. If the client has seen a `stored_at` (returned when the escrow is stored) newer than its copy, refresh *before* comparing.
- `replaced` distinguishes first setup from rotation; only rotation shows the old-artifact guidance.

### Schedule and Triggers

- **7 days** after setup, then **90 days**, backing off to **180 days** after two consecutive successes.
- **Re-arm** to 7 days on: new device enrolled (prompt on the new device), secret rotated, restore from escrow.
- Snooze 24 h or 7 d, at most 3 in a row. Delivered as the pre-armed `recovery_check_due` alert class ([](/design/notifications/#alert-classes)); [](/design/notifications/) owns snooze and badge mechanics. The alert's `recovery` parameter is `check` or `rewrap`; an explicit "I lost it" is due immediately.

### On Repeated Failure: Guided Re-Wrap

After 3 failures across ≥2 sessions, or an explicit "I lost it":

1. Mint a fresh ≥128-bit secret and **re-wrap the same master key**.
2. Replace the server escrow (old blob deleted).
3. Repeat the type-back gate.
4. Re-issue Shamir shares if enrolled (old ones marked invalid).
5. Show the old-artifact guidance.

This is **wrap rotation, not key rotation**: no re-encryption and no blob-hash changes. Master-key rotation is the compromise procedure in [](/design/cryptography/keys/).

## Backup Verification

Restore is **dry-run by default**; writing requires explicit `--commit` (UI: typed-phrase confirm after the report).

| Stage | Checks | Writes |
| --- | --- | --- |
| Preview | Counts, sizes, readable titles | No |
| Dry-run (default) | Decrypt, hash match, decode in the sandboxed decoder ([](/design/clients/#sandboxed-decoder)); diff vs live library | No |
| Signature chain | `MANIFEST.cbor` HMAC + exporter signature; every asset manifest ([](/design/cryptography/provenance/#asset-manifest)) against the device directory, every device cert to a user IK. Any break refuses restore | No |
| AMK completeness | Every referenced `amk_version` present in the artifact's own ledger | No |
| Commit | Chain reconciliation below | Yes |

Commit reconciles each manifest against the live `latest_provenance_hash` and **never silently overwrites newer local state**:

- **Identical head**: no-op (idempotent).
- **Live head chains forward from restored head**: not applied; offered read-only as an older version.
- **Divergent, behind, or tombstoned later locally**: not applied; sent to the "restore conflicts" quarantine ([](/design/threat-model/scenarios/#quarantine-surfaces); Damage Scenario #23 in [](/design/threat-model/scenarios/#damage-scenario--invariant-map)).
- **Absent locally**: applied; the restored chain becomes the local chain.

## Backup Provenance

`MANIFEST.cbor` records exporter device, export time, source library version, `crypto_suite_id` and every per-asset chain head; it is itself a library-level provenance record ([](/design/cryptography/provenance/#provenance-of-library-modifications)). Restore re-injects per-asset chains, so a restored library knows it was restored, when and by whom.

## Failure Modes

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| Master key loss | — | Path 1 or 2 ([](#recovery-paths)) |
| Backup missing an `amk_version` | AMK-completeness check ([](#backup-verification)) | Re-export before relying on the backup |

## Validation

- Local verify is offline and side-effect free; stale-cache refresh after rotation; cadence and re-arm under a mocked clock.
- Guided re-wrap keeps the master key (hashes unchanged) and rejects the old escrow.
- Single-root tripwire: no second secret class in the schema.
- Artifact round-trip; byte-identical re-export; deterministic dry-run report.
- Tampering is caught by HMAC, then signature, then directory membership; a missing AMK version is caught at dry-run.
- Each recovery path has one scenario: reach the lost state, recover, assert every asset returns. Each reconciliation case is tested; an old backup never overwrites later deletes or edits.
- An exported artifact contains no MLS ratchet state.

The [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs) covers full backup → fresh-client restore, and push → `auth recover` onto a fresh library → `restore`, where every album's originals come back byte for byte, every epoch included.
