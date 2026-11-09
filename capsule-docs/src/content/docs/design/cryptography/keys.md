---
title: Key Management
description: Capsule's key hierarchy, device coordination, and write authorization
status: draft
---

One hierarchy with one backed-up root, implemented in `capsule-core/src/crypto/keys/`. Every signer sits behind one seam, so software and hardware keys are interchangeable.

- The **account master key** is the only escrowed root and never encrypts assets. It wraps device private keys and seals the escrows of every album's keys and the portable backup's AMK ledger.
- **Device keys** are hardware-bound and disposable: a lost device is removed and a new one bootstrapped from the master key.
- **AMKs** are random per-epoch album keys, ledgered in MLS. They are escrowed server-side under the master key ([](/design/backup-recovery/#album-key-escrow)), in the backup artifact, and in the OGK ([](#owner-group-keys-ogks)).

**Invariant:** the backup path is independent of the MLS ratchet. Losing every device but holding the recovery passphrase restores every photo ([](/design/backup-recovery/#recovery-paths)).

## Key Chain

```mermaid title="Key hierarchy: the master key wraps downward to the per-encryption file key"
flowchart TB
    MK["account_master_key<br/>escrowed"]:::accent
    DEV["Device private keys<br/>IK / DSK / DEK"]:::client
    AID["Default album_id<br/>identifier, not a key"]
    AMK["AMK_v{n}<br/>random 32 B, per album, per epoch"]
    FK["Per-encryption file key"]
    STREAM["AES-256-GCM-STREAM"]
    MK -->|wraps| DEV
    MK -->|derives| AID
    MK -->|seals in album escrow<br/>and backup AMK ledger| AMK
    AMK -->|derives| FK
    FK -->|keys| STREAM
```

Each album has one server-side key escrow. Rules, with the KDF choice in [](/design/cryptography/primitives/#key-derivation):

- `info` MUST carry a version string. Salts MUST be unique per scope (`album_id`, `file_id`) and never reused across scopes.
- Every **encryption** derives a fresh key (`nonce_prefix` is folded into the salt), so even a same-`file_id` `replace` ([](/design/authorization/#the-closed-action-set)) re-rolls the key. No `(file_key, nonce_prefix)` pair is ever reused. The formula is in [](/design/cryptography/encryption/#asset-key-derivation).
- **File-key modes:** *derived* (default, nothing stored) and *wrapped* (asset key wrapped under the AMK). Wrapped is only for bytes the client did not author, i.e. an adopted web-upload drop ([](/design/web-upload/)). The signed `key_mode` ([](/design/cryptography/provenance/#asset-manifest)) records which. The mode never affects authorization and `verify_asset` ([](#write-authorization)) ignores it.
- The master key derives the default album's `album_id` (HKDF, dedicated label; see [](/design/organization/#the-default-album)) so any device, including a recovered one, can find it.
- AMKs are never derived from MLS ratchet state, which is ephemeral.

## Key Generation

All keys are generated client-side from the OS CSPRNG ([](/design/cryptography/primitives/#randomness)). Every asymmetric key is hybrid classical + PQ, and both halves must verify.

### User Identity Keys (User IKs)

- A hybrid **Ed25519 + ML-DSA-65** signing keypair, drawn once per account at account creation. It is the root of trust and signs the [](#device-directory).
- Random, **not** derived from the master key: master rotation MUST NOT rotate identity. Private halves are sealed under the master key ([](#registered-accounts)).
- Verified out-of-band or by safety numbers.
- Revocation is a global account reset: an IK-signed revocation certificate at a well-known location. Irreversible.

### Device Keys

| Key | Algorithm | Purpose |
| --- | --- | --- |
| DSK | Ed25519 + ML-DSA-65 | Signs manifests and commits as this device |
| DEK | X25519 + ML-KEM-768 | Receives encapsulated keys |

- The IK cross-signs both public halves via the [](#device-directory).
- The **classical half** is generated in hardware and never leaves it (Secure Enclave, StrongBox/Keystore, TPM via Windows TBS). No shipping secure element implements PQ, so the **PQ half** is software-sealed under hardware-protected material.
- Secure elements expose only **P-256**: Secure Enclave is P-256 only, StrongBox mandates P-256 with no Curve25519, TPM 2.0 mandates P-256. The hardware composition is therefore the **P-256 hybrid variant** ([](/design/device-enrollment/)). Devices without a usable element (TPM 1.2, none) use the software keystore. RSA is rejected.
- **Revocation** is an IK-signed revocation statement plus an MLS `Remove` of the device's leaves. The server stops delivering wraps to it and the remaining devices rotate the group keys it held. The directory entry is **retained** with `revoked_at`, never deleted, so its pre-revocation signatures stay verifiable ([](/design/cryptography/provenance/#what-an-attacker-with-all-current-keys-still-cannot-do)).

### Owner Group Keys (OGKs)

**Deferred post-v1.** Nothing implements the owner MLS group; master-key escrow is the v1 recovery path. Also deferred: library-settings keying ([](/design/metadata/#the-library-settings-document)), OGK-escrow recovery and Drop-Key multi-device re-wrap.

- One MLS group per `owner_id` (its user set), with an AES-256 group root key.
- It escrows every album's AMK versions ([](#album-master-keys-amks)), never individual file keys, so any owner member can recover every asset independent of album membership.
- The epoch bumps on any owner-set change. Removal is an MLS `Remove`, after which the removed user gets no future OGK or AMK escrow.

### Album Master Keys (AMKs)

One MLS group per album, whose members are users with any permission. Secrecy (encryption) is separated from authorization (signatures):

| Material | Holders | Use |
| --- | --- | --- |
| **AMK**: random 32 B per epoch, distributed over MLS application messages | All members | Decrypt (read) |
| **Write-tier keypair**: hybrid Ed25519 + ML-DSA-65 ([](/design/cryptography/primitives/#signature-scheme)), per epoch | Writers only | Sign asset manifests ([](/design/cryptography/provenance/#asset-manifest)) |
| **Admin-tier keypair**: hybrid Ed25519 + ML-DSA-65 | Admins only | Sign MLS membership commits |

The epoch bumps on member add or remove, permission change, and scheduled rotation (e.g. 30 days).

## Write Authorization

A DSK signature proves *which device* produced an asset, not that it was *authorized to write* at that epoch. The server is not trusted for authorization, since it may replay, reorder or surface reader-signed or removed-writer assets, and bugs must fail closed.

`verify_asset(manifest, ciphertext_hash, authority)` in `capsule-core/src/crypto/verify_asset.rs` is the **single chokepoint** by which a client acknowledges an asset on import, sync, federation, peering and restore. Rules:

- **Epoch-bound write proof.** Every manifest carries a DSK signature **and** a write-tier signature for its `amk_version` epoch, verified against the write-tier public key the authority ([](#write-authority-interface)) attests for that epoch. A server assertion is never sufficient.
- **Epoch ceiling is MLS-attested.** The client's monotonic `amk_version` ceiling comes from the admin-signed MLS commit chain ([](/design/cryptography/mls/#membership-operations)), never from the server's counter. A new client learns it from its Welcome ([](/design/cryptography/mls/#history-delivery-for-new-joiners)). The server's own monotonicity check ([](/design/threat-model/validation/#server-side-validation-invariants)) is only a backstop against clients skipping epochs.
- **Accepts and rejects.** An asset written by a writer later removed is accepted, because it was valid when written. One signed at an epoch where the signer was not a writer is rejected.
- **Backdating buys nothing.** Authorization and ordering ride the epoch and provenance chain. A removed writer holds no write-tier key for any later epoch, and old epochs fail the monotonic `amk_version` and chain-head checks.
- **Downgrade-resistant.** Both signatures cover `crypto_suite_id`, `protocol_version` and `prior_provenance_hash`.
- **Hash, not bytes.** It takes the ciphertext hash computed by the streaming path and never buffers a file.
- **Three outcomes:**
  - *accept*;
  - *terminal-reject* (reader-signed, removed-writer, wrong-epoch, forged chain, suite-downgrade, tampered field): quarantined and surfaced in the audit trail ([](/design/cryptography/provenance/#provenance-of-library-modifications)), never silently dropped;
  - *pending*: `amk_version` is within the attested range but the AMK has not yet arrived over AlbumKeyDistribution ([](/design/cryptography/mls/#history-delivery-for-new-joiners)). Held and retried, escalating to quarantine after a timeout.
- **Key delivery is authorization-neutral.** A `key_mode = wrapped` asset is authorized identically, by the adopter's `device_sig` + `write_sig`; the guest never signs. `key_mode` and `wrapped_file_key` are signed fields, so tampering is terminal-reject.
- **Timestamp is audit-only.** The manifest `timestamp` is never used for authorization or ordering. Time-based policy uses the server's `received_at` (server envelope: [](/design/filesystem/server/#postgresql-what-the-server-knows)). The server's drift bound on `timestamp` is a sanity check, not a control. The grammar is in [](/design/threat-model/schema-rules/#timestamp-grammar).
- **Validity ≠ durability.** Local destructive cleanup requires `verify_asset` **and** the key-free durability endpoint ([](/design/transfer/durability/)).

### Write Authority Interface

`verify_asset` takes `&dyn AlbumAuthority` (`capsule-core/src/crypto/authority/`) and asks only for:

- the album id;
- the attested **epoch ceiling**;
- the **write-tier public key** per attested epoch;
- whether the AMK for an epoch is **locally held** (pending vs. reject);
- whether the admin attestation chain verifies.

Two implementations:

- `ReferenceAuthority`: an admin-signed epoch ledger with no MLS dependency. The offline and test authority on every build.
- `OpenMlsAuthority`: the design target, over live OpenMLS group state ([](/design/cryptography/mls/)). Host-only.

"MLS-attested" in these docs means "attested by the album's `AlbumAuthority`".

## Device Directory

Each user publishes an IK-signed directory, which peers read to learn which device keys to trust. Shape (`capsule-core/src/crypto/keys/directory.rs`):

```rust
DeviceDirectory {
  user_id,
  directory_version: u64,   // strictly monotonic, +1 on every change
  updated_at: RFC3339,
  devices: [{ device_id, dsk_public, dek_public?, added_at, revoked_at? }],
  signature: Hybrid(IK)     // over the canonical-CBOR core
}
```

- **Anti-rollback.** Every reader (a client, a federated peer, a peering handshake; see [](/design/federation/) and [](/design/peering/)) caches the highest `directory_version` per user and **refuses and surfaces** a lower one. With no cached version it trusts on first use, then pins. Own-account first contact is corroborated over the enrollment channel ([](/design/device-enrollment/)), cross-user first contact by safety numbers. It is enforced as client- and server-side invariants ([](/design/threat-model/validation/#client-side-validation-invariants), [](/design/threat-model/validation/#server-side-validation-invariants)). The manifest-level counterpart is [](/design/sync/#stale-revival-detection).
- **Server identity anchor.** The document carries the IK's signature but not the IK, so publish sends the IK public key beside it in `X-Capsule-Identity-Key`. The first publish pins it (trust-on-first-publish). Every later publish MUST present the same key and verify under it, else `403` with no detail about the stored anchor. Global sign-out ([](/design/authentication/#explicit-revocation)) depends on this anchor.
- **Field presence is signature-visible.** Optional fields (e.g. `dek_public`) encode as an **absent key**, never present-`null`, so directories signed before a field existed still verify. These are the same wire-presence rules as for manifests ([](/design/cryptography/provenance/#asset-manifest)).
- **`added_at` bound.** The server rejects an upload whose manifest `timestamp` predates the signing device's `added_at`.
- Adding a user to an album adds all their non-revoked listed devices. MLS orders concurrent commits and the loser re-proposes.

## Identity-Based Key Derivation

How the master key and identity are held depends on the account type ([](/design/authentication/#account-types)).

### Registered accounts

- The master key is generated client-side and the server never holds it in the clear. Each device stores it wrapped under its DEK.
- A new device obtains it by cross-device recovery ([](/design/backup-recovery/#recovery-mechanisms)) or by unwrapping the server-side escrow ([](/design/backup-recovery/#master-key-escrow)) with the recovery passphrase.
- The IK is stored **sealed under the master key** (domain-separated) beside the escrow, at `/v1/auth/escrow/identity`. There is one per account, replaced atomically and independent of the escrow: recovery-secret rotation leaves it untouched.
- Each album's keys are escrowed beside it at `/v1/auth/escrow/albums/{album_id}`, also sealed under the master key (domain-separated): every epoch's AMK, the album's signing seeds, and its authority. There is one slot per album, re-stored before any asset of a new album or epoch is pushed ([](/design/backup-recovery/#album-key-escrow)).
- Neither the sealed identity nor the album escrows add a second root.
- Recovery unwraps the escrow, opens the sealed identity, creates the device library from both, and adopts every escrowed album, all before anything resolves the default album. An escrow without a sealed identity cannot recover as itself: `error.escrow.identity_not_stored`, remedied by finishing backup from a device holding the IK.

### Delegated/Sponsored accounts

1. **Per-sponsoree KEK:** random 32 B, wrapped under the sponsor's master key. It is never derived, because a derivation would let the sponsor reproduce it after revocation.
2. The sponsoree's own IK, device and album keys are generated normally, with private halves wrapped under the KEK.
3. Shared-album access is ordinary MLS membership. The KEK is not a content key.
4. **Revocation**, all IK-signed by the sponsor: rotate the KEK (re-wrapping survivors), publish a revocation certificate (checked by clients and federated peers), and MLS-`Remove` the sponsoree's devices everywhere (bumping AMK epochs). The sponsor's master key is untouched.

**Recovery** is only through the sponsor. The sponsoree has no passphrase or escrow of its own, so a sponsor who loses their master key loses every sponsoree's data.

#### Damage bound under sponsor compromise

A compromised sponsor holds every sponsoree KEK. It can impersonate the sponsoree's devices, rewrite its provenance history ([](/design/cryptography/provenance/#provenance-of-library-modifications)) and write to its albums. This is bounded, not eliminated:

- The trust is explicit and directional, so use sponsorship only where it already holds.
- The blast radius stops at that sponsor's sponsorees (per-sponsoree KEKs). Other users verify the sponsoree's directory ([](#device-directory)) and provenance like any peer.
- Users needing integrity independent of a sponsor must hold a registered account.

### Non-registered accounts

- **Reading** ([](/design/share-links/)): decryption keys are encapsulated under the link secret, optionally plus a password-based KDF layer ([](/design/cryptography/primitives/#password-based-kdf)).
- **Writing into an album: not supported.** There is no MLS membership and no write-tier key, so no valid manifest.
- **Contributing a drop** ([](/design/web-upload/)): the provisioning client mints a per-link **Drop Key** (hybrid X25519 + ML-KEM-768, as a DEK; see [](#device-keys)). The public half goes in the URL fragment. The private half is wrapped under the provisioner's master key (and OGK once it exists). Guests encapsulate a random per-asset key to it. A trusted client adopts the drop ([](/design/web-upload/#drop-and-adoption-lifecycle)), re-wrapping that key under the AMK (wrapped mode, [](#key-chain)). Deleting the link revokes the Drop Key.

## Key Rotation and Revocation

- **Master key:** rotation re-wraps device wraps, the AMK escrow and the sealed identity ([](#registered-accounts)), then discards the old key. Signed-in sessions are unaffected.
- **Device:** revocation certificate ([](#device-keys)) + MLS `Remove` ([](/design/cryptography/mls/#membership-operations)).
- **Album member:** MLS `Remove` + AMK epoch bump.

### Master-Key Compromise

The hierarchy wraps downward and is never inverted: wrapping AMKs under disposable device keys would forfeit recovery. Response:

1. Rotate the master key.
2. Re-key affected albums. The epoch bump mints fresh AMKs and write-tier keys ([](/design/mls-resilience/#group-re-keying-ceremony)).

Ciphertext already exfiltrated under old AMKs stays readable to the attacker. Forward writes are clean.

## Failure Modes

Paths are the numbered [](/design/backup-recovery/#recovery-paths). Rejected manifests and distribution lag are the `verify_asset` outcomes above ([](#write-authorization)). Master-key loss is in [](/design/backup-recovery/#failure-modes).

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| Device key loss | Device keys are disposable | Re-bootstrap (path 1 or 2); never recovered |
| AMK loss | — | Path 3 or 4 |
| Write-tier key loss | — | Re-minted next epoch |
| Master key compromise | — | Rotate and re-key ([](#master-key-compromise)) |
| Device compromise | — | Revocation certificate ([](#device-keys)) + MLS `Remove`; survivors rotate |
| AMK / write-tier compromise | — | Epoch bump; an old epoch cannot read or sign future ones |
| Server compromise | Server never trusted | Authorization from MLS history; data is end-to-end encrypted |
| One half of a hybrid primitive broken | Hybrid construction | The other half holds |

## Validation

- Every failure-mode row tests its detection and recovery.
- **Derivation determinism:** HKDF output is byte-identical across platforms; RFC 5869 vectors.
- **Hardware round-trip:** per platform, generate a DSK in hardware, sign and verify. Reading private bytes MUST fail. The desktop leg is Windows TBS.
- **Directory field wire-absence:** unset optional fields leave the signed core byte-identical to a pre-field fixture. Set fields are signature-covered and tampering fails.
- **Rotation ceremony:** rotate against a real escrow. Every device wrap and AMK escrow entry is still recoverable.
- **`verify_asset` negatives:** reader-signed, removed-writer, wrong-epoch, forged chain, replay, suite-downgrade, half-valid hybrid `write_sig` ([](/design/cryptography/provenance/)).
- **`verify_asset` pending:** an unheld AMK in range is pending, delivering it accepts, and a version beyond the ceiling is terminal-reject.
- **Directory monotonicity:** `N-1` is refused and surfaced; `N+1` is accepted and pinned.
