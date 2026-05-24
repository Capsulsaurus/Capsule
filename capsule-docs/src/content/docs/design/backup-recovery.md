---
title: Backup and Recovery
description: How Capsule backs up libraries and recovers them after device or key loss
---

Capsule treats loss of data — and loss of the keys that decrypt it — as a first-class failure mode to design against. Recovery rests on a single rule:
holding the recovery secret must restore every asset, even after every device is lost. This document consolidates the artifacts and mechanisms that uphold it.

Two distinct things are called a "backup" here, and they are kept separate on purpose:

- The **encrypted backup artifact** — a portable, encrypted export of a library's assets.
- The **master-key escrow** — a small server-side blob that lets a passphrase reconstruct the key hierarchy.

## Backup Artifact

A backup is a single self-describing, versioned, **streamable** archive containing everything needed to restore a library's assets. It is itself encrypted and kept independent of the device key hierarchy, so recovery does not depend on reconstructing MLS ratchet state (see [Cryptography](/design/cryptography/#failure-modes-and-recovery)).

A backup is an export artifact — not part of the live library or the server blob store — and may be stored locally or on external storage such as hard drives or cloud storage. It is used to restore assets after data loss or when setting up a new device. The format is versioned to allow future improvements and changes without breaking older backups.

### Container Format

The container is an **uncompressed POSIX tar** with deterministic entry ordering and a top-level signed integrity manifest:

- **Uncompressed.** Asset ciphertext is incompressible (it's the output of [AES-256-GCM-STREAM](/design/cryptography/#bulk-aead)); compressing it buys nothing and adds CPU cost. Metadata blobs are likewise encrypted before they hit the archive, so the same applies.
- **Streamable.** Tar is append-friendly and has no central directory, so a backup of arbitrary size can be written and read end-to-end without seeking — important when exporting a terabyte-scale library to spinning rust or an external drive.
- **Deterministic ordering.** Entries are written in sorted order by `(album_id, asset_id, blob_role)`, so two exports of the same logical content produce byte-identical archives. This lets the integrity manifest's signature verify across re-exports.
- **Top-level integrity manifest.** The first entry is `MANIFEST.cbor` — a CBOR document listing every entry's path, [content hash](/design/cryptography/#primitives-inventory), declared size, and the exporting device's identity. The manifest is authenticated **two ways**:
  - An **HMAC** keyed by the backup's wrap key (derived from the user passphrase via the [password-based KDF](/design/cryptography/#password-based-kdf)) catches truncation, reordering, and corruption *before* any decrypt is attempted.
  - A **hybrid Ed25519 + ML-DSA-65 signature** from the exporting device's [DSK](/design/cryptography/#device-keys) — the same [signature scheme](/design/cryptography/#signature-scheme) used for asset manifests. The signature defeats a symmetric-key attacker who could otherwise re-HMAC after tampering: an attacker who steals the wrap key can re-HMAC but cannot forge the device signature.
  Both checks must pass before restore proceeds. The signing device must be present in the user's [device directory](/design/cryptography/#per-user-device-coordination) at restore time; an exporter device that was later revoked is rejected.
- **Versioned.** A `VERSION` entry pins the artifact format version, `crypto_suite_id`, and `min_protocol_version` per [Versioning](/design/versioning/) and [Cryptography — Versioning Identifiers](/design/cryptography/#versioning-identifiers). Older backup artifacts remain restorable by newer Capsule versions; an artifact whose `crypto_suite_id` is not in the current inventory is rejected at restore (per [Threat Model — Schema Evolution](/design/threat-model/#schema-evolution-and-field-grammar)).

ZIP was considered and rejected: its central-directory-at-end makes streaming writes awkward at terabyte scale, ZIP64 tooling support is inconsistent, and there is no compression benefit to gain from ZIP-internal deflate.

## Master-Key Escrow

The account master key is the single backed-up root of the key hierarchy (see [Cryptography](/design/cryptography/#key-management)). It is escrowed server-side so a user holding only their recovery secret can reconstruct it:

- Wrap the account master key with a user-chosen high-entropy passphrase or a randomly generated 48+ bit recovery code.
- Derive the wrapping key with the [password-based KDF](/design/cryptography/#password-based-kdf). Store the wrapped blob server-side.
- If you can run enclaves (SGX/Nitro/SEV-SNP), do Signal's SVR trick: rate-limit PIN attempts inside the enclave so a weak PIN is still safe. Without enclaves, require a real passphrase or recovery code — don't let users pick 4-digit PINs.

## Recovery Mechanisms

Two recovery mechanisms ship by default; a third is available opt-in for users who want extra redundancy without compromising the default's simplicity.

### Default mechanisms

- **Recovery passphrase / BIP39-style seed** shown at setup; the user prints it or stores it in a password manager. It unwraps the master-key escrow above.
- **Cross-device recovery** — any existing signed-in device can re-bootstrap a new one over a verified channel.

(We need at least two for redundancy; the third below is opt-in to keep the default flow simple.)

### Opt-in: Shamir Secret Sharing

Users who want to spread recovery across trusted parties or storage locations can enable **Shamir Secret Sharing** of the recovery seed. The default scheme is **2-of-3**:

- The recovery seed (the same one that unwraps the master-key escrow) is split into 3 shares; any 2 reconstruct the seed; 1 alone reveals nothing.
- Each share is itself wrapped with a per-share passphrase via the [password-based KDF](/design/cryptography/#password-based-kdf), so storing a share on a less-trusted medium (cloud drive, second device, trusted family member) is safer.
- Reconstruction happens fully client-side. Capsule's server never sees more than one share at a time and never sees a reconstructed seed.
- Custom `m`-of-`n` (e.g. 3-of-5 for users who want broader distribution) is supported but not the default.

This is the social-recovery escape hatch — useful for users who would otherwise lose access from a single forgotten passphrase plus a single dead device.

## Backup Verification

A restore that overwrites live state silently is the worst foot-gun a backup system can ship. Capsule therefore makes **dry-run the default**: a `restore` invocation runs in dry-run mode unless the user passes an explicit `--commit` flag (or its UI equivalent: a confirm-with-typed-phrase dialog after the dry-run report is shown). The mode hierarchy is:

- **Preview mode (always safe).** Verify the shape of your content makes sense — counts, sizes, asset titles where readable. No decrypt, no write.
- **Dry-run mode (default for `restore`).** Verify everything can be decrypted, matches its hashes, and (as a sanity check) that images and videos decode properly in the [sandboxed decoder](/design/clients/#sandboxed-decoder). Compute the diff against the current live library: what would be added, what would conflict, what would be skipped as already present. No write.
- **Signature-chain verification.** Every [asset manifest](/design/cryptography/#provenance-and-signed-manifest) verifies against the published [device directory](/design/cryptography/#per-user-device-coordination), and every device certificate chains to a user IK. The MANIFEST.cbor itself must verify both HMAC and exporter signature (above). Any break is flagged and the restore is refused.
- **AMK completeness check.** Confirm every `amk_version` referenced by an asset is present in the backup, so no asset is silently unrecoverable.
- **Commit (only with explicit consent).** The user reviews the dry-run report and explicitly commits. Even at commit, the restore obeys the [stale-revival defense](/design/import-synchronization/#stale-revival-detection): a restored manifest whose `prior_provenance_hash` conflicts with the live library's current chain head goes to the [quarantine surface](/design/threat-model/#quarantine-surfaces) and the user resolves it explicitly. The interaction between backup restore and the stale-revival defense is flagged as an [open question](/design/threat-model/#open-questions) — the resolution will land here before the docs ship.

## Backup Provenance

The MANIFEST.cbor carries the exporter's device id, the export timestamp, the source library version, the `crypto_suite_id` at export time, and a list of every provenance-chain head per asset included in the backup. The MANIFEST is itself a [provenance record](/design/cryptography/#provenance-of-library-modifications) at the library level: who exported, when, from what device. A successful restore re-injects each per-asset provenance chain into the restored library, so the audit trail survives the round-trip — a restored library knows it was restored, from when, by whom.
