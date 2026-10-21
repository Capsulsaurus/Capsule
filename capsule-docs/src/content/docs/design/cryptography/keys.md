---
title: Key Management
description: Capsule's key hierarchy, device coordination, and write authorization
status: draft
---

One hierarchy, one backed-up root; implemented in `capsule-core/src/crypto/keys/`. Every signer sits behind one seam, so software and hardware keys are interchangeable.

- The **account master key** is the only escrowed key. It never encrypts assets; it wraps device private keys and anchors the encrypted backup that escrows AMKs.
- **Device keys** are hardware-bound and disposable: a lost device is removed and a new one bootstrapped from the master key.
- **AMKs** are random per-epoch album keys, ledgered in MLS and escrowed in the master-key backup and the [OGK](#owner-group-keys-ogks).

**Invariant:** the backup path is independent of the MLS ratchet — losing every device but holding the recovery passphrase restores every photo ([Failure Modes](/design/cryptography/failure-modes/)).

## Key Chain

```plaintext
account_master_key (escrowed)
  ├─ wraps device private keys (IK / DSK / DEK)
  ├─ derives default album_id (an identifier, not a key)
  └─ anchors the encrypted backup escrowing:
        AMK_v{n}  (random 32 B, per album, per MLS epoch)
          └─ per-encryption file key (Encryption — Asset Key Derivation)
                └─ AES-256-GCM-STREAM
```

Rules (with the [KDF choice](/design/cryptography/primitives/#key-derivation)):

- `info` MUST carry a version string; salts MUST be unique per scope (`album_id`, `file_id`) and never reused across scopes.
- Every **encryption** derives a fresh key (`nonce_prefix` is folded into the salt), so even a same-`file_id` [`replace`](/design/authorization/#the-closed-action-set) re-rolls the key. No `(file_key, nonce_prefix)` pair is ever reused. Formula: [Encryption — Asset Key Derivation](/design/cryptography/encryption/#asset-key-derivation).
- **File-key modes:** *derived* (default, nothing stored) and *wrapped* (asset key wrapped under the AMK, only for bytes the client did not author, i.e. an adopted [web-upload drop](/design/web-upload/)). The signed [`key_mode`](/design/cryptography/provenance/#asset-manifest) records which. The mode never affects authorization; [`verify_asset`](#write-authorization) ignores it.
- The master key derives the [default album](/design/organization/#the-default-album)'s `album_id` (HKDF, dedicated label) so any device, including a recovered one, can find it.
- AMKs are never derived from MLS ratchet state, which is ephemeral.

## Key Generation

All keys are generated client-side from the [OS CSPRNG](/design/cryptography/primitives/#randomness). Every asymmetric key is hybrid classical + PQ; both halves must verify.

### User Identity Keys (User IKs)

- Hybrid **Ed25519 + ML-DSA-65** signing keypair, drawn at account creation, once per account. Root of trust; signs the [device directory](#device-directory).
- Random, **not** derived from the master key (master rotation MUST NOT rotate identity). Private halves are sealed under the master key ([Registered accounts](#registered-accounts)).
- Verified out-of-band / by safety numbers.
- Revocation = global account reset: an IK-signed revocation certificate at a well-known location. Irreversible.

### Device Keys

| Key | Algorithm | Purpose |
| --- | --- | --- |
| DSK | Ed25519 + ML-DSA-65 | Signs manifests and commits as this device |
| DEK | X25519 + ML-KEM-768 | Receives encapsulated keys |

- Both public halves are cross-signed by the IK via the [device directory](#device-directory).
- The **classical half** is generated in and never leaves hardware (Secure Enclave, StrongBox/Keystore, TPM via Windows TBS); the **PQ half** is software-sealed under hardware-protected material, since no shipping secure element implements PQ.
- Secure elements expose only **P-256** (Secure Enclave: P-256 only; StrongBox: P-256 mandatory, no Curve25519; TPM 2.0: P-256 mandatory), so the hardware composition is the **P-256 hybrid variant** ([Device Enrollment](/design/device-enrollment/)). Devices without a usable element (TPM 1.2, none) use the software keystore; RSA is rejected.
- **Revocation:** IK-signed revocation statement + MLS `Remove` of the device's leaves; the server stops delivering wraps to it and remaining devices rotate the group keys it held. The directory entry is **retained** with `revoked_at`, never deleted, so its pre-revocation signatures stay verifiable ([Provenance](/design/cryptography/provenance/#what-an-attacker-with-all-current-keys-still-cannot-do)).

### Owner Group Keys (OGKs)

**Deferred post-v1.** Nothing implements the owner MLS group; master-key escrow is the v1 recovery path. Also deferred: [library-settings](/design/metadata/#the-library-settings-document) keying, OGK-escrow recovery, Drop-Key multi-device re-wrap.

- One MLS group per `owner_id` (its user set); AES-256 group root key.
- Escrows every album's [AMK versions](#album-master-keys-amks) — never individual file keys — so any owner member can recover every asset independent of album membership.
- Epoch bumps on any owner-set change; removal → MLS Remove → removed user gets no future OGK or AMK escrow.

### Album Master Keys (AMKs)

One MLS group per album; members are users with any permission. Secrecy (encryption) is separated from authorization (signatures):

| Material | Holders | Use |
| --- | --- | --- |
| **AMK** — random 32 B per epoch, distributed over MLS application messages | All members | Decrypt (read) |
| **Write-tier keypair** — hybrid Ed25519 + ML-DSA-65 ([signature scheme](/design/cryptography/primitives/#signature-scheme)), per epoch | Writers only | Sign [asset manifests](/design/cryptography/provenance/#asset-manifest) |
| **Admin-tier keypair** — hybrid Ed25519 + ML-DSA-65 | Admins only | Sign MLS membership commits |

Epoch bumps on member add/remove, permission change, and scheduled rotation (e.g. 30 days).

## Write Authorization

A DSK signature proves *which device* produced an asset, not that it was *authorized to write* at that epoch. The server is not trusted for authorization (it may replay, reorder or surface reader-signed / removed-writer assets), and bugs must fail closed.

`verify_asset(manifest, ciphertext_hash, authority)` in `capsule-core/src/crypto/verify_asset.rs` is the **single chokepoint** by which a client acknowledges an asset — on import, sync, federation, peering and restore. Rules:

- **Epoch-bound write proof.** Every manifest carries a DSK signature **and** a write-tier signature for its `amk_version` epoch, verified against the write-tier public key the [authority](#write-authority-interface) attests for that epoch. A server assertion is never sufficient.
- **Epoch ceiling is MLS-attested.** The client's monotonic `amk_version` ceiling comes from the admin-signed [MLS commit chain](/design/cryptography/mls/#membership-operations) (a new client learns it from its [Welcome](/design/cryptography/mls/#history-delivery-for-new-joiners)), never from the server's counter. The server's own monotonicity check ([Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants)) is only a backstop against clients skipping epochs.
- **Accepts / rejects.** Written by a writer later removed → accepted (valid when written). Signed at an epoch where the signer was not a writer → rejected.
- **Backdating buys nothing.** Authorization and ordering ride the epoch and provenance chain; a removed writer holds no write-tier key for any later epoch, and old epochs fail the monotonic `amk_version` + chain-head checks.
- **Downgrade-resistant.** Both signatures cover `crypto_suite_id`, `protocol_version` and `prior_provenance_hash`.
- **Hash, not bytes.** Takes the ciphertext hash computed by the streaming path; never buffers a file.
- **Three outcomes:**
  - *accept*;
  - *terminal-reject* (reader-signed, removed-writer, wrong-epoch, forged chain, suite-downgrade, tampered field) → quarantined and surfaced in the [audit trail](/design/cryptography/provenance/#provenance-of-library-modifications), never silently dropped;
  - *pending* — `amk_version` within the attested range but the AMK has not yet arrived over [AlbumKeyDistribution](/design/cryptography/mls/#history-delivery-for-new-joiners). Held and retried; escalates to quarantine after a timeout ([Failure Modes](/design/cryptography/failure-modes/#failure-mode-catalog)).
- **Key delivery is authorization-neutral.** A `key_mode = wrapped` asset is authorized identically (adopter's `device_sig` + `write_sig`; the guest never signs). `key_mode` and `wrapped_file_key` are signed fields; tampering is terminal-reject.
- **Timestamp is audit-only.** The manifest `timestamp` is never used for authorization or ordering; time-based policy uses the server's `received_at` ([server envelope](/design/filesystem/server/#postgresql-what-the-server-knows)). The server's drift bound on `timestamp` is a sanity check, not a control. Grammar: [Schema Rules](/design/threat-model/schema-rules/).
- **Validity ≠ durability.** Local destructive cleanup requires `verify_asset` **and** the key-free [Transfer — Durability endpoint](/design/transfer/durability/).

### Write Authority Interface

`verify_asset` takes `&dyn AlbumAuthority` (`capsule-core/src/crypto/authority/`) and asks only: the album id, the attested **epoch ceiling**, the **write-tier public key** per attested epoch, whether the AMK for an epoch is **locally held** (pending vs. reject), and whether the admin attestation chain verifies.

- `ReferenceAuthority` — admin-signed epoch ledger, no MLS dependency; offline/test authority on every build.
- `OpenMlsAuthority` — the design target, over live OpenMLS group state ([MLS](/design/cryptography/mls/)); host-only.

"MLS-attested" in these docs means "attested by the album's `AlbumAuthority`".

## Device Directory

Each user publishes an IK-signed directory; peers read it to learn which device keys to trust. Shape (`capsule-core/src/crypto/keys/directory.rs`):

```rust
DeviceDirectory {
  user_id,
  directory_version: u64,   // strictly monotonic, +1 on every change
  updated_at: RFC3339,
  devices: [{ device_id, dsk_public, dek_public?, added_at, revoked_at? }],
  signature: Hybrid(IK)     // over the canonical-CBOR core
}
```

- **Anti-rollback.** Every reader (client, [federated](/design/federation/) peer, [peering](/design/peering/) handshake) caches the highest `directory_version` per user and **refuses and surfaces** a lower one. No cached version → TOFU, then pin. Own-account first contact is corroborated over the [enrollment channel](/design/device-enrollment/); cross-user first contact by safety numbers. Enforced as [client-](/design/threat-model/validation/#client-side-validation-invariants) and [server-side](/design/threat-model/validation/#server-side-validation-invariants) invariants; the manifest-level counterpart is [stale-revival detection](/design/sync/#stale-revival-detection).
- **Server identity anchor.** The document carries the IK's signature but not the IK, so publish sends the IK public key beside it in `X-Capsule-Identity-Key`. The first publish pins it (trust-on-first-publish); every later publish MUST present the same key and verify under it, else `403` with no detail about the stored anchor. [Global sign-out](/design/authentication/#explicit-revocation) depends on this anchor.
- **Field presence is signature-visible.** Optional fields (e.g. `dek_public`) encode as an **absent key**, never present-`null`, so directories signed before a field existed still verify — the same [wire-presence rules](/design/cryptography/provenance/#asset-manifest) as manifests.
- **`added_at` bound.** The server rejects an upload whose manifest `timestamp` predates the signing device's `added_at`.
- Adding a user to an album adds all their non-revoked listed devices; concurrent commits are ordered by MLS (loser re-proposes).

## Identity-Based Key Derivation

How the master key and identity are held depends on the [account type](/design/authentication/#account-types).

### Registered accounts

- Master key generated client-side; the server never holds it in the clear. Each device stores it wrapped under its DEK.
- A new device obtains it by [cross-device recovery](/design/backup-recovery/#recovery-mechanisms) or by unwrapping the [server-side escrow](/design/backup-recovery/#master-key-escrow) with the recovery passphrase.
- The IK is stored **sealed under the master key** (domain-separated) beside the escrow, at `/v1/auth/escrow/identity`: one per account, replaced atomically, independent of the escrow (recovery-secret rotation leaves it untouched). It adds no second root.
- Recovery = unwrap escrow → open sealed identity → create the device library from both. Escrow without a sealed identity cannot recover as itself: `error.escrow.identity_not_stored` (remedy: finish backup from a device holding the IK).

### Delegated/Sponsored accounts

1. **Per-sponsoree KEK** — random 32 B (never derived: a derivation would let the sponsor reproduce it after revocation), wrapped under the sponsor's master key.
2. The sponsoree's own IK/device/album keys are generated normally; private halves are wrapped under the KEK.
3. Shared-album access is ordinary MLS membership; the KEK is not a content key.
4. **Revocation** (all IK-signed by the sponsor): rotate the KEK (re-wrap survivors), publish a revocation certificate (checked by clients and federated peers), MLS-Remove the sponsoree's devices everywhere (AMK epoch bumps). The sponsor's master key is untouched.

**Recovery** is only through the sponsor: no passphrase or escrow of its own, so a sponsor who loses their master key loses every sponsoree's data.

#### Damage bound under sponsor compromise

A compromised sponsor holds every sponsoree KEK and can impersonate the sponsoree's devices, rewrite its [provenance history](/design/cryptography/provenance/#provenance-of-library-modifications) and write to its albums. Bounded, not eliminated:

- The trust is explicit and directional — use sponsorship only where it already holds.
- Blast radius stops at that sponsor's sponsorees (per-sponsoree KEKs); other users verify the sponsoree's [directory](#device-directory) and provenance like any peer.
- Users needing integrity independent of a sponsor must hold a registered account.

### Non-registered accounts

- **Reading** ([share links](/design/share-links/)): decryption keys are encapsulated under the link secret, optionally plus a [password-based KDF](/design/cryptography/primitives/#password-based-kdf) layer.
- **Writing into an album: not supported** — no MLS membership, no write-tier key, so no valid manifest.
- **Contributing a drop** ([Web Upload](/design/web-upload/)): the provisioning client mints a per-link **Drop Key** (hybrid X25519 + ML-KEM-768, as a [DEK](#device-keys)); public half in the URL fragment, private half wrapped under the provisioner's master key (and OGK once it exists). Guests encapsulate a random per-asset key to it; a trusted client [adopts](/design/web-upload/#drop-and-adoption-lifecycle) the drop, re-wrapping that key under the AMK ([wrapped mode](#key-chain)). Deleting the link revokes the Drop Key.

## Key Rotation and Revocation

- **Master key:** rotation re-wraps device wraps, the AMK escrow and the [sealed identity](#registered-accounts), then discards the old key. Signed-in sessions are unaffected.
- **Device:** [revocation certificate](#device-keys) + MLS `Remove` ([MLS — Membership Operations](/design/cryptography/mls/#membership-operations)).
- **Album member:** MLS `Remove` + AMK epoch bump.

### Master-Key Compromise

The hierarchy wraps downward and is never inverted (wrapping AMKs under disposable device keys would forfeit recovery). Response:

1. Rotate the master key.
2. Re-key affected albums — epoch bump mints fresh AMKs and write-tier keys ([MLS Resilience](/design/mls-resilience/#group-re-keying-ceremony)).

Already-exfiltrated ciphertext under old AMKs stays readable to the attacker; forward writes are clean.

## Validation

- **Derivation determinism** — HKDF output byte-identical across platforms; RFC 5869 vectors.
- **Hardware round-trip** — per platform: generate DSK in hardware, sign, verify; reading private bytes MUST fail. Desktop leg is Windows TBS.
- **Directory field wire-absence** — unset optional fields leave the signed core byte-identical to a pre-field fixture; set fields are signature-covered and tamper fails.
- **Rotation ceremony** — rotate against a real escrow; every device wrap and AMK escrow entry still recoverable.
- **`verify_asset` negatives** — reader-signed, removed-writer, wrong-epoch, forged chain, replay, suite-downgrade, half-valid hybrid `write_sig` ([Provenance](/design/cryptography/provenance/)).
- **`verify_asset` pending** — unheld AMK in range → pending; deliver → accept; beyond ceiling → terminal-reject.
- **Directory monotonicity** — `N-1` refused and surfaced; `N+1` accepted and pinned.
