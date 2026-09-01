---
title: Backup and Recovery
description: The portable backup artifact, the master-key escrow, and the recovery flows
status: draft
---

Capsule treats loss of data — and loss of the keys that decrypt it — as a first-class failure mode. Recovery rests on a single rule: holding the recovery secret must restore every asset, even after every device is lost. This document defines the two artifacts and the mechanisms that uphold it.

The live server is the everyday backstop a device leans on between explicit backups: a device that has uploaded an asset may release its local copy and re-fetch on demand. That reliance is not an unverified assumption about durable hardware — before discarding the only local copy a client confirms, via the [storage-verification endpoint](/design/import/storage-verification/), that the server durably holds the bytes (stored, indexed, retrievable), so "the server has it" is a checked fact.

Two distinct things are called a "backup" here, and they are kept separate on purpose:

- The **[backup artifact](#backup-artifact)** — a portable, encrypted export of a library's assets.
- The **[master-key escrow](#master-key-escrow)** — a small server-side blob that lets a passphrase reconstruct the key hierarchy.

The artifact format is the contract that backup-restore implementations on every platform must conform to byte-for-byte (else a backup made on one device could not be restored on another). Implemented in `capsule-core::backup` — export, container assembly, manifest signing, and the inverse restore path — and called by per-platform UI flows.

## Backup Artifact

A backup is a single self-describing, versioned, **streamable** archive containing everything needed to restore a library's assets. It is itself encrypted and kept independent of the device key hierarchy, so recovery does not depend on reconstructing MLS ratchet state (see [Cryptography — Failure Modes](/design/cryptography/failure-modes/)).

A backup is an export artifact — not part of the live library or the server blob store — and may be stored locally or on external storage such as hard drives or cloud storage. It is used to restore assets after data loss or when setting up a new device. The format is versioned to allow future improvements without breaking older backups.

### Container Format

The container is an **uncompressed POSIX tar** with deterministic entry ordering and a top-level signed integrity manifest:

```text
backup.tar
├── VERSION                # plaintext: artifact-format version, crypto_suite_id, min_protocol_version
├── MANIFEST.cbor          # CBOR: entry list, hashes, sizes, exporter identity; HMAC + hybrid signature
├── keys/
│   └── amk-ledger.cbor    # every album's AMK versions needed to decrypt the included assets,
│                          #   wrapped under the backup wrap key (derived from the recovery passphrase)
└── <entries, sorted by (album_id, asset_id, blob_role)>
    ├── blobs/{hash}       # encrypted ciphertext blobs
    ├── meta/{blob_id}     # encrypted metadata blobs
    └── provenance/{asset_id}  # full per-asset provenance chains
```

The artifact carries its own [AMK](/design/cryptography/keys/#album-master-keys-amks) ledger so it is **self-sufficient**: a holder of the recovery passphrase can decrypt `amk-ledger.cbor` and then every included blob, without contacting the server or reconstructing MLS ratchet state. The ledger is wrapped under the backup wrap key (same passphrase-derived key that authenticates `MANIFEST.cbor`), not under any device key — this is what makes the [AMK-completeness check](#backup-verification) a check the artifact can answer about *itself* rather than a promise about a separate server-side blob.

The container properties below are what make it both safe and portable:

- **Uncompressed.** Asset ciphertext is incompressible (it is the output of [AES-256-GCM-STREAM](/design/cryptography/primitives/#bulk-aead)); compressing it buys nothing and adds CPU cost. Metadata blobs are likewise encrypted before they hit the archive, so the same applies.
- **Streamable.** Tar is append-friendly and has no central directory, so a backup of arbitrary size can be written and read end-to-end without seeking — important when exporting a terabyte-scale library to spinning rust or an external drive.
- **Deterministic ordering.** Entries are written in sorted order by `(album_id, asset_id, blob_role)`, so two exports of the same logical content produce byte-identical archives. This lets the integrity manifest's signature verify across re-exports. Determinism also requires **normalized tar headers** — `mtime = 0`, `uid = gid = 0`, empty user/group names, fixed mode bits, plain ustar with no PAX extension headers — so every entry's bytes are a pure function of its content and path, never of the exporting machine.
- **Top-level integrity manifest.** Written before any blob entry (right after the tiny `VERSION` header), `MANIFEST.cbor` lists every entry's path, [content hash](/design/cryptography/primitives/), declared size, and the exporting device's identity — so a streaming reader holds the full integrity list before the first blob arrives. The manifest is authenticated **two ways**:
  - An **HMAC** keyed by the backup's wrap key (derived from the user passphrase via the [password-based KDF](/design/cryptography/primitives/#password-based-kdf)) catches truncation, reordering, and corruption *before* any decrypt is attempted.
  - A **hybrid Ed25519 + ML-DSA-65 signature** from the exporting device's [DSK](/design/cryptography/keys/#device-keys) — the same [signature scheme](/design/cryptography/primitives/#signature-scheme) used for asset manifests. The signature defeats a symmetric-key attacker who could otherwise re-HMAC after tampering: an attacker who steals the wrap key can re-HMAC but cannot forge the device signature.
  Both checks must pass before restore proceeds. The signing device must be present in the user's [device directory](/design/cryptography/keys/#device-directory) at restore time; an exporter device that was later revoked is rejected.
- **Versioned.** The `VERSION` entry pins the artifact format version, `crypto_suite_id`, and `min_protocol_version` per [Versioning](/design/versioning/) and [Cryptography — Versioning Identifiers](/design/cryptography/primitives/#versioning-identifiers). Older backup artifacts remain restorable by newer Capsule versions; an artifact whose `crypto_suite_id` is not in the current inventory is rejected at restore (per [Threat Model — Schema Rules](/design/threat-model/schema-rules/)).

ZIP was considered and rejected: its central-directory-at-end makes streaming writes awkward at terabyte scale, ZIP64 tooling support is inconsistent, and there is no compression benefit to gain from ZIP-internal deflate.

## Master-Key Escrow

The account master key is the single backed-up root of the key hierarchy (see [Cryptography — Keys](/design/cryptography/keys/)). It is escrowed server-side so a user holding only their recovery secret can reconstruct it:

- Wrap the account master key with a user-chosen high-entropy passphrase or a randomly generated recovery code of **≥128 bits** (e.g. a BIP39-style phrase) — the same entropy floor as every other unguessable identifier in the system. The escrow blob is offline-attackable once exfiltrated, and [Argon2id](/design/cryptography/primitives/#password-based-kdf) raises brute-force cost only linearly, so the secret itself must carry the security: **no low-entropy code is permitted without an enclave** (below).
- Derive the wrapping key with the [password-based KDF](/design/cryptography/primitives/#password-based-kdf). Store the wrapped blob server-side.
- If you can run enclaves (SGX/Nitro/SEV-SNP), Signal's SVR pattern is the *only* sanctioned way to soften that floor: rate-limit unwrap attempts inside the enclave so a shorter, human-friendly PIN is still safe. Without an attested enclave, the ≥128-bit floor is mandatory — never a 4-digit PIN, never a short numeric code.

## Single-Root Invariant

**Every recovery path terminates at the one account master key, and the recovery secret (or a quorum of its Shamir shares) is the only user-held secret class. No feature may introduce a second escrowed secret class or a recovery path that bypasses the master key.**

This is already true; the invariant fixes it against drift. The audit, stated so nobody has to re-derive it: the server **escrow** is the master key wrapped by the recovery secret (above); the **backup artifact's** wrap key derives from that same recovery passphrase; **Shamir** splits that same seed; the [OGK](/design/cryptography/keys/#owner-group-keys-ogks) and Drop-Key escrows are wraps *reachable from* the master key, not additional roots; [sponsored accounts](/design/authentication/#account-types) deliberately hold no root of their own — every path routes through the sponsor's, which is why the verification cadence below never prompts a sponsoree. These are **wraps, not roots** — holding the recovery secret reaches all of them; losing everything but the recovery secret loses nothing.

One versioning seam to know about, not remove: an already-exported backup artifact stays bound to the passphrase in force at export. Rotating the recovery secret (below) therefore ends with "re-export or destroy old artifacts" guidance — a property of offline artifacts, not a second secret class.

## Recovery Mechanisms

Two recovery mechanisms ship by default; a third is available opt-in for users who want extra redundancy without compromising the default's simplicity. These complement the [seven independent recovery paths](/design/cryptography/failure-modes/#redundant-recovery-paths); this section names the mechanisms a user actually invokes.

### Default Mechanisms

- **Recovery passphrase / BIP39-style seed** shown at setup; the user prints it or stores it in a password manager. It unwraps the master-key escrow above.
- **Cross-device recovery** — any existing signed-in device can re-bootstrap a new one over a verified channel. The first-device-ever flow is owned by [Device Enrollment](/design/device-enrollment/).

(We need at least two for redundancy; the third below is opt-in to keep the default flow simple.)

### Opt-in: Shamir Secret Sharing

Users who want to spread recovery across trusted parties or storage locations can enable **Shamir Secret Sharing** of the recovery seed. The default scheme is **2-of-3**:

- The recovery seed (the same one that unwraps the master-key escrow) is split into 3 shares; any 2 reconstruct the seed; 1 alone reveals nothing.
- Each share is itself wrapped with a per-share passphrase via the [password-based KDF](/design/cryptography/primitives/#password-based-kdf), so storing a share on a less-trusted medium (cloud drive, second device, trusted family member) is safer.
- Reconstruction happens fully client-side. Capsule's server never sees more than one share at a time and never sees a reconstructed seed.
- Custom `m`-of-`n` (e.g. 3-of-5 for users who want broader distribution) is supported but not the default.

This is the social-recovery escape hatch — useful for users who would otherwise lose access from a single forgotten passphrase plus a single dead device.

## Recovery Verification Cadence

A recovery secret that was written on a napkin thirteen months ago is a recovery secret the user *believes* they have. The client therefore occasionally asks the user to **verify** it — and the check is designed so it can never itself become a lock-out or a guessing oracle.

### Local Verification

Verification is **local-only**: the client keeps a cached copy of the escrow blob (fetched at enrollment, refreshed opportunistically via sync); the user enters the passphrase; the client runs the [password KDF](/design/cryptography/primitives/#password-based-kdf), unwraps the cached blob (`capsule_core::backup::verify_recovery_secret`), and compares an HKDF-derived tag (`capsule-recovery-verify/v1`) against the same tag derived from the device-held master key — a derived-tag compare, never raw key bytes. No server round-trip: it works offline, creates no server-side guessing surface, and a failure can't lock anything.

**Stale-cache rule:** before recording a failure, the client refreshes the escrow blob from the server (it may have been rotated from another device) and retries once — otherwise every rotation would manufacture false failures on the user's other devices.

### Schedule and Triggers

- **7 days** after setup (catches the lost-napkin case while re-setup is cheap), then **90 days**, backing off to a **180-day cap** after two consecutive successes.
- **Re-arm triggers** (reset to the 7-day step): a new device enrolls (the prompt lands on the *new* device — it has never seen the passphrase); the recovery secret rotates; a restore-from-escrow completes.
- Snooze steps are 24 h or 7 d, at most 3 consecutive. This is the `recovery_check_due` [alert class](/design/notifications/#alert-classes): the bounded-snooze-then-badge mechanic, and the rule that no alert blocks a critical flow, are owned by [Notifications](/design/notifications/); the cadence and re-arm triggers above are owned here. Because each step is a deadline the device can compute, the prompt is **pre-armed** ([pre-arm rule](/design/notifications/#the-pre-arm-rule)) rather than evaluated at launch.

### On Repeated Failure: Guided Re-Wrap

After 3 failures across ≥ 2 app sessions — or an explicit "I lost it" — the client runs the guided rotation flow: mint a fresh ≥128-bit recovery secret, **re-wrap the same master key**, replace the server escrow (a single active escrow; the old blob is deleted, so the lost secret unwraps nothing), re-run the setup-style type-back gate, re-issue Shamir shares if enrolled (old shares explicitly invalidated and surfaced as such), and surface the old-backup-artifact guidance from the [single-root invariant](#single-root-invariant).

**This is wrap rotation, not key rotation**: an O(1) escrow-blob replacement with no data re-encryption and no blob-hash changes. Rotating the master key itself remains the separate compromise procedure owned by [Cryptography — Keys](/design/cryptography/keys/).

## Backup Verification

A restore that overwrites live state silently is the worst foot-gun a backup system can ship. Capsule therefore makes **dry-run the default**: a `restore` invocation runs in dry-run mode unless the user passes an explicit `--commit` flag (or its UI equivalent: a confirm-with-typed-phrase dialog after the dry-run report is shown). The mode hierarchy:

- **Preview mode (always safe).** Verify the shape of your content makes sense — counts, sizes, asset titles where readable. No decrypt, no write.
- **Dry-run mode (default for `restore`).** Verify everything can be decrypted, matches its hashes, and (as a sanity check) that images and videos decode properly in the [sandboxed decoder](/design/clients/#sandboxed-decoder). Compute the diff against the current live library: what would be added, what would conflict, what would be skipped as already present. No write.
- **Signature-chain verification.** Every [asset manifest](/design/cryptography/provenance/#asset-manifest) verifies against the published [device directory](/design/cryptography/keys/#device-directory), and every device certificate chains to a user IK. The MANIFEST.cbor itself must verify both HMAC and exporter signature (above). Any break is flagged and the restore is refused.
- **AMK completeness check.** Decrypt `keys/amk-ledger.cbor` and confirm every `amk_version` referenced by any included asset is present in it, so no asset is silently unrecoverable. Because the ledger ships *inside* the artifact, this check is answerable from the artifact alone — it does not depend on a separate server-side escrow blob that could have drifted.
- **Commit (only with explicit consent).** The user reviews the dry-run report and explicitly commits. Even at commit, a restore **never silently overwrites newer local state** — it is a chain-reconciliation, not a blind overwrite. Each restored manifest is reconciled against the live library's `latest_provenance_hash` for its `asset_id`:
  - **Identical head** → no-op; already current (restore is idempotent).
  - **Live head chains *forward* from the restored head** → the live copy is newer; the older restored manifest is **not applied**. It is surfaced read-only ("an older version exists in this backup") so the user may deliberately roll back, but nothing is overwritten automatically.
  - **Divergent, behind, or locally tombstoned at a later step** → **not applied**; the restored manifest goes to the ["restore conflicts" quarantine surface](/design/threat-model/scenarios/#quarantine-surfaces) for explicit user merge. A six-month-old backup therefore cannot resurrect an asset the user later deleted, nor clobber edits made after the backup was taken — directly addressing [Damage Scenario #23](/design/threat-model/scenarios/#damage-scenario--invariant-map).
  - **Asset absent locally** → applied directly; the restored provenance chain becomes the local chain.

  This is the committed resolution of the former restore-vs-stale-revival question: the conservative default is that newer local state always wins unless the user explicitly chooses an older version, and no restore is ever silently destructive.

## Backup Provenance

The MANIFEST.cbor carries the exporter's device id, the export timestamp, the source library version, the `crypto_suite_id` at export time, and a list of every provenance-chain head per asset included in the backup. The MANIFEST is itself a [provenance record](/design/cryptography/provenance/#provenance-of-library-modifications) at the library level: who exported, when, from what device. A successful restore re-injects each per-asset provenance chain into the restored library, so the audit trail survives the round-trip — a restored library knows it was restored, from when, by whom.

## Validation

- **Local verify round-trip (unit).** Offline: the correct passphrase verifies with zero network I/O; a wrong one fails without side effects. Rotate the escrow out-of-band; assert the stale-cache rule refreshes before recording a failure and then passes with the new passphrase.
- **Cadence schedule (unit).** Under a mocked clock: 7 d → 90 d → 180 d backoff; re-arm on device-add, rotation, and restore; snooze caps; never blocks a critical flow.
- **Guided re-wrap (smoke).** After the failure threshold: a new escrow wraps the *same* master key (fixture assets still decrypt; blob hashes unchanged), the old escrow is rejected everywhere, Shamir re-issue is prompted where enrolled, and the old-artifact guidance is surfaced.
- **Single-root audit (unit).** A doc-driven test walks the seven recovery paths and asserts each terminates at the master key; a tripwire asserts no second escrowed secret class exists in the schema.
- **Artifact round-trip (unit).** Export → import a small library; assert byte-equal blob set, sidecars, and provenance chains. Determinism check: re-export the same library twice; assert byte-identical archives.
- **MANIFEST verification (unit).** Tamper individual entries; assert HMAC mismatch detected. Tamper MANIFEST itself and re-HMAC; assert exporter-signature mismatch detected. Strip the exporter from the device directory; assert restore refusal.
- **AMK-completeness check (unit).** Build an artifact whose `keys/amk-ledger.cbor` is deliberately missing an `amk_version` that an included asset references; assert detection at dry-run, before any commit. Build a self-sufficient artifact; assert every included asset decrypts from the artifact's own ledger with no server contact.
- **Per-recovery-path smoke** (passphrase, cross-device, Shamir 2-of-3): each is a separate scenario that ends with the library restored on a fresh device.
- **Dry-run determinism (smoke).** Run dry-run twice against an unchanged backup + library; assert byte-identical diff report.
- **Restore reconciliation (smoke).** Exercise each reconciliation case and assert the outcome: identical head → no-op; live head ahead of the restored head → restored *not* applied (offered read-only); divergent or locally-tombstoned-later → quarantined for explicit merge, never silent overwrite; asset absent locally → applied. A six-month-old backup restored over a library with subsequent deletes and edits leaves no live state overwritten.

The cross-module case — full backup → full restore on a fresh client → verify every asset readable — is one bounded E2E case in [Module Map](/design/module-map/#e2e-test-surface).
