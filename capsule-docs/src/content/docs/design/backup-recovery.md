---
title: Backup and Recovery
description: The portable backup artifact, the master-key escrow, and the recovery flows
status: draft
---

**Rule:** holding the recovery secret restores every asset, even after every device is lost.

Two separate things are called "backup":

- The **[backup artifact](#backup-artifact)** — a portable, encrypted export of a library's assets.
- The **[master-key escrow](#master-key-escrow)** — a small server-side blob a passphrase unwraps into the key hierarchy.

A client MAY drop its only local copy only after the [Transfer — Durability endpoint](/design/transfer/durability/) confirms the server durably holds it.

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
    ├── provenance/{asset_id}  # full per-asset provenance chains
    ├── authority/{album_id}   # admin verifying key + admin-signed epoch ledger (format 2+)
    └── docs/{album|stack}/{id} # album/stack label record, sealed under its album's AMK (optional)
```

- **Self-sufficient.** The [AMK](/design/cryptography/keys/#album-master-keys-amks) ledger is wrapped under the passphrase-derived backup wrap key, not a device key: the passphrase alone decrypts every included blob, with no server or MLS state.
- **Authority is public, not a capability.** `authority/` makes restored assets *verifiable*; it carries no write-tier or admin seed, so an album new to the library restores read-only. Restore re-verifies the ledger's admin signature chain, adopts it only for an album with no authority of its own, and never replaces existing authority. Format-1 artifacts restore readable but unverifiable.
- **Label records ride sealed.** `docs/` carries each exported album's [`AlbumDocV1`](/design/organization/#labels) (its name and labels) and each labelled stack's `StackDocV1`, still sealed under the album's AMK, whose epoch the ledger escrows. Restore merges them, so a recovered album keeps its name. The entries are additive within format 2: an older reader verifies and skips them.
- **Key pins ride verbatim.** `library/attestation-keys.cbor` carries the library's pinned attestation-key history of each server ([Custody Receipts](/design/transfer/durability/#custody-receipts)), so a restored library keeps checking receipts under the pinned keys instead of trusting the server's next publication on first use. Restore keeps the union per server only where one history extends the other; histories that each hold a key the other lacks refuse the restore before anything is written. A pin whose schema is newer than the restoring build reads is skipped with a warning, never a reason to refuse the assets. Additive within format 2: an older reader verifies and skips it.
- **Uncompressed** (ciphertext is incompressible) and **streamable** (no central directory; terabyte exports without seeking — why not ZIP).
- **Deterministic.** Sorted entries and normalized ustar headers (`mtime`/`uid`/`gid` = 0, empty names, fixed modes, no PAX) — re-exports of the same content are byte-identical.
- **Manifest first.** `MANIFEST.cbor` follows `VERSION` and precedes every blob, so a streaming reader has the integrity list up front. Both checks MUST pass before restore:
  - **HMAC** under the wrap key ([password-based KDF](/design/cryptography/primitives/#password-based-kdf)) — catches truncation, reordering, corruption before decrypt.
  - **Hybrid [signature](/design/cryptography/primitives/#signature-scheme)** by the exporter's [DSK](/design/cryptography/keys/#device-keys) — defeats a wrap-key thief who re-HMACs. The exporter MUST be in the [device directory](/design/cryptography/keys/#device-directory) at restore; a revoked exporter is rejected.
- **Versioned** per [Versioning Identifiers](/design/cryptography/primitives/#versioning-identifiers): old artifacts stay restorable; an unknown `crypto_suite_id` is rejected.

## Master-Key Escrow

The account master key ([Keys](/design/cryptography/keys/)) is escrowed server-side, wrapped by a key derived from the recovery secret via the [password-based KDF](/design/cryptography/primitives/#password-based-kdf).

- The secret MUST carry **≥128 bits** (random code or BIP39-style phrase): the blob is offline-attackable and Argon2id adds only linear cost.
- A shorter PIN is permitted **only** behind an attested enclave that rate-limits unwraps (Signal SVR pattern). Never a short numeric code otherwise.
- One active escrow per account; replacing it deletes the old blob.

CLI (`capsule auth escrow` / `capsule auth recover`): setup binds the library to the signed-in account and refuses a library that already holds assets (its manifests name its own id); it shows a fresh 256-bit secret once behind a type-back gate; it refuses to replace an existing escrow without `--replace`. Every refusal writes nothing.

### Album Key Escrow

The master key and the [sealed identity](/design/cryptography/keys/#registered-accounts) open a recovered library as the account, but [AMKs](/design/cryptography/keys/#album-master-keys-amks) are random per album and epoch. Each album's key state is therefore escrowed beside the master-key escrow, so a device recovered from the recovery secret alone can open every album.

- **What is escrowed.** One blob per album: every epoch's AMK, the album's write-tier and admin seeds, and its authority, prefixed with a domain separator and sealed under the master key. It adds no root ([Single-Root Invariant](#single-root-invariant)). A recovered device holds the album as the device that escrowed it did, so it can still write to the album.
- **Where.** `PUT /v1/auth/escrow/albums/{album_id}` stores or replaces one album's slot. `GET /v1/auth/escrow/albums` lists every slot the account holds. Slots are opaque, per account, replaced in one operation, never versioned, never deleted, and capped at 512 KiB each.
- **Kept current before any asset leaves.** Setup escrows every album. Every push first lists the escrows, then re-escrows each album the server holds missing or stale (a new album, or a new epoch). It does this before any asset is sent, and also when there is nothing to push. A refused escrow fails the push. The server must never hold an asset whose album key it does not also hold.
- **Adopted before any album is resolved.** `capsule auth recover` fetches the escrows before deriving anything. Once the library is created, it adopts every escrowed album before anything calls `ensure_album`. That ordering is what stops a recovered library minting a fresh AMK under the derived default-album id, which the account's other devices already wrote under.
- **Failure handling.** An escrow that opens but is inconsistent refuses the recovery, and the new library is removed. Inconsistent means it is served under another album's id, or it is not an album escrow, or its authority does not re-verify. An escrow that does not open under the recovered master key is skipped: another library sealed it before the escrow was replaced. Those albums belong to that library's key, and refusing them would lock the account out for good.
- **Known limit.** One slot per album, so the last writer wins. If two devices that both hold an album each rotate it independently, they replace each other's escrow. Reconciling diverging epochs waits on the MLS membership work.

## Single-Root Invariant

**Every recovery path terminates at the one account master key, and the recovery secret (or a quorum of its Shamir shares) is the only user-held secret class. No feature may add a second escrowed secret class or a path that bypasses the master key.**

The escrow, the artifact wrap key, and Shamir shares all derive from the same secret. OGK and Drop-Key escrows, the [album key escrows](#album-key-escrow) and the [sealed user identity](/design/cryptography/keys/#registered-accounts) are wraps *reachable from* the master key, not roots. [Sponsored accounts](/design/authentication/#account-types) hold no root; they route through the sponsor's (so they are never prompted to verify).

An exported artifact stays bound to the secret in force at export; rotating the secret therefore ends with "re-export or destroy old artifacts" guidance.

## Recovery Mechanisms

These are the user-invoked mechanisms among the [redundant recovery paths](/design/cryptography/failure-modes/#redundant-recovery-paths).

### Default Mechanisms

- **Recovery secret** shown at setup; unwraps the escrow.
- **Cross-device recovery** — a surviving signed-in device re-bootstraps a new one over a verified channel (ceremony shared with [cross-device add](/design/device-enrollment/#cross-device-add)).

### Opt-in: Shamir Secret Sharing

- The recovery seed is split **2-of-3** by default (custom `m`-of-`n` allowed); fewer than `m` shares reveal nothing.
- Each share is additionally wrapped under its own passphrase via the password-based KDF.
- Reconstruction is client-side only; the server never sees more than one share or the seed.

#### Share Wire Format

A share is `x ‖ y`: one nonzero byte `x`, then one GF(256) byte per seed byte (33 bytes for a 32-byte seed), byte-wise Shamir over polynomial `0x11d` with uniformly random coefficients from a CSPRNG. Malformed, zero-`x`, or duplicate-`x` shares are a typed error, never a panic. Shares issued by the retired `sharks` crate (RUSTSEC-2024-0398, ~0.18 bits leaked per share) use the same encoding and stay recoverable; [guided re-wrap](#on-repeated-failure-guided-re-wrap) re-issues them.

## Recovery Verification Cadence

Periodically the user proves they still hold the secret. The check MUST NOT become a lock-out or a guessing oracle.

### Local Verification

- **Local-only.** The client unwraps a cached copy of the escrow blob and compares an HKDF-derived tag against one derived from the device-held master key (never raw key bytes). No server round-trip; a failure locks nothing.
- **Stale cache.** Before recording a failure, refresh the blob and retry once. If the client has seen a `stored_at` (from `PUT /v1/auth/escrow`) newer than its copy, refresh *before* comparing.
- `replaced` distinguishes first setup from rotation; only rotation shows the old-artifact guidance.

### Schedule and Triggers

- **7 days** after setup, then **90 days**, backing off to **180 days** after two consecutive successes.
- **Re-arm** to 7 days on: new device enrolled (prompt on the new device), secret rotated, restore from escrow.
- Snooze 24 h or 7 d, at most 3 in a row. Delivered as the pre-armed `recovery_check_due` [alert class](/design/notifications/#alert-classes); snooze/badge mechanics are owned by [Notifications](/design/notifications/). The alert's `recovery` parameter is `check` or `rewrap` (an explicit "I lost it" is due immediately).

### On Repeated Failure: Guided Re-Wrap

After 3 failures across ≥2 sessions, or an explicit "I lost it": mint a fresh ≥128-bit secret, **re-wrap the same master key**, replace the server escrow (old blob deleted), repeat the type-back gate, re-issue Shamir shares if enrolled (old ones marked invalid), and show the old-artifact guidance.

This is **wrap rotation, not key rotation** — no re-encryption, no blob-hash changes. Master-key rotation is the compromise procedure in [Keys](/design/cryptography/keys/).

## Backup Verification

Restore is **dry-run by default**; writing requires explicit `--commit` (UI: typed-phrase confirm after the report).

| Stage | Checks | Writes |
| --- | --- | --- |
| Preview | Counts, sizes, readable titles | No |
| Dry-run (default) | Decrypt, hash match, decode in the [sandboxed decoder](/design/clients/#sandboxed-decoder); diff vs live library | No |
| Signature chain | `MANIFEST.cbor` HMAC + exporter signature; every [asset manifest](/design/cryptography/provenance/#asset-manifest) against the device directory, every device cert to a user IK. Any break refuses restore | No |
| AMK completeness | Every referenced `amk_version` present in the artifact's own ledger | No |
| Commit | Chain reconciliation below | Yes |

Commit reconciles each manifest against the live `latest_provenance_hash` and **never silently overwrites newer local state**:

- **Identical head** → no-op (idempotent).
- **Live head chains forward from restored head** → not applied; offered read-only as an older version.
- **Divergent, behind, or tombstoned later locally** → not applied; sent to the ["restore conflicts" quarantine](/design/threat-model/scenarios/#quarantine-surfaces) ([Damage Scenario #23](/design/threat-model/scenarios/#damage-scenario--invariant-map)).
- **Absent locally** → applied; the restored chain becomes the local chain.

## Backup Provenance

`MANIFEST.cbor` records exporter device, export time, source library version, `crypto_suite_id`, and every per-asset chain head; it is itself a library-level [provenance record](/design/cryptography/provenance/#provenance-of-library-modifications). Restore re-injects per-asset chains, so a restored library knows it was restored, when, and by whom.

## Validation

- Local verify is offline and side-effect free; stale-cache refresh after rotation; cadence and re-arm under a mocked clock.
- Guided re-wrap keeps the master key (hashes unchanged) and rejects the old escrow.
- Single-root tripwire: no second secret class in the schema.
- Artifact round-trip; byte-identical re-export; deterministic dry-run report.
- Tampering is caught by HMAC, then signature, then directory membership; a missing AMK version is caught at dry-run.
- Each recovery path and each reconciliation case; an old backup never overwrites later deletes or edits.

Full backup → fresh-client restore is an E2E case in [Module Map](/design/module-map/#e2e-test-surface). So is push → `auth recover` onto a fresh library → `restore`: every album's originals come back byte for byte, every epoch included.
