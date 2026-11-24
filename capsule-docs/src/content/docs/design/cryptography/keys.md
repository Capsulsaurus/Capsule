---
title: Key Management
description: Capsule's key hierarchy, device coordination, and write authorization
audit: none
---

One hierarchy with one backed-up root, implemented in `capsule-core/src/crypto/keys/`. Every signer sits behind one seam, so software and hardware keys are interchangeable.

- The **account master key** is the only escrowed root and never encrypts assets. It wraps device private keys and seals the escrows of every album's keys and the portable backup's AMK ledger.
- **Device keys** are hardware-bound and disposable: a lost device is removed and a new one bootstrapped from the master key.
- **AMKs** are per-epoch album keys, exported from the album's MLS epoch and ledgered. They are escrowed server-side under the master key ([](/design/backup-recovery/#album-key-escrow)), in the backup artifact, and in the OGK ([](#owner-group-keys-ogks)).

**Invariant:** the backup path is independent of the MLS ratchet. Losing every device but holding the recovery passphrase restores every photo ([](/design/backup-recovery/#recovery-paths)).

## Key Chain

```mermaid title="Key hierarchy: the master key wraps downward to the per-encryption file key"
flowchart TB
    MK["account_master_key<br/>escrowed"]:::accent
    DEV["Device private keys<br/>IK / DSK / DEK"]:::client
    AID["Default album_id<br/>identifier, not a key"]
    AMK["AMK_v{n}<br/>32 B, per album, per epoch<br/>exported from MLS, then stored"]
    FK["Per-encryption file key"]
    STREAM["AES-256-GCM-STREAM"]
    MK -->|wraps| DEV
    MK -->|derives once,<br/>at account creation| AID
    MK -->|seals in album escrow<br/>and backup AMK ledger| AMK
    AMK -->|derives| FK
    FK -->|keys| STREAM
```

Each album has one server-side key escrow. Rules, with the KDF choice in [](/design/cryptography/primitives/#key-derivation):

- `info` MUST carry a version string. Salts MUST be unique per scope (`album_id`, `file_id`) and never reused across scopes.
- Every **encryption** derives a fresh key (`nonce_prefix` is folded into the salt), so even a same-`file_id` `replace` ([](/design/authorization/#the-closed-action-set)) re-rolls the key. No `(file_key, nonce_prefix)` pair is ever reused. The formula is in [](/design/cryptography/encryption/#asset-key-derivation).
- **File-key modes:** _derived_ (default, nothing stored) and _wrapped_ (asset key wrapped under the AMK). Wrapped is only for bytes the client did not author, i.e. an adopted web-upload drop ([](/design/web-upload/)). The signed `key_mode` ([](/design/cryptography/provenance/#asset-manifest)) records which. The mode never affects authorization and `verify_asset` ([](#write-authorization)) ignores it.
- The default album's `album_id` is derived from the account's **first** master key at account creation (HKDF, label `default-album-id/v1`; see [](/design/organization/#the-default-album)). From then on it is a stored value: it is sealed in the identity record beside the IK ([](#registered-accounts)) and re-sealed with it on every master-key rotation, so a recovered device reads it rather than computing it. Nothing re-derives it from the current master key: after a rotation that derivation names a different, empty album.
- An AMK is exported from MLS once and then stored; nothing re-derives an AMK from MLS state afterwards, because MLS deletes epoch secrets ([](#album-master-keys-amks)).

## Key Generation

All keys are generated client-side from the OS CSPRNG ([](/design/cryptography/primitives/#randomness)). Every asymmetric key is hybrid classical + PQ, and both halves must verify. The exception is the project's release keys, which no user holds ([](#project-release-keys)).

### User Identity Keys (User IKs)

- A hybrid **Ed25519 + ML-DSA-65** signing keypair, drawn once per account at account creation. It is the root of trust and signs the [](#device-directory).
- Random, **not** derived from the master key: master rotation MUST NOT rotate identity. Private halves are sealed under the master key ([](#registered-accounts)).
- Verified out-of-band or by safety numbers.
- **Rotation is rooted in the recovery secret** ([](#identity-rotation)), the one secret no device stores, so an IK an attacker has copied can be replaced.
- Revocation is a global account reset: an IK-signed revocation certificate at a well-known location. Irreversible, and the last resort when the recovery secret is lost as well.

### Device Keys

| Key | Algorithm           | Purpose                                    |
| --- | ------------------- | ------------------------------------------ |
| DSK | Ed25519 + ML-DSA-65 | Signs manifests and commits as this device |
| DEK | X25519 + ML-KEM-768 | Receives encapsulated keys                 |

- The IK cross-signs both public halves via the [](#device-directory).
- The **classical half** is generated in hardware and never leaves it (Secure Enclave, StrongBox/Keystore, TPM via Windows TBS). No shipping secure element implements PQ, so the **PQ half** is software-sealed under hardware-protected material.
- Secure elements expose only **P-256**: Secure Enclave is P-256 only, StrongBox mandates P-256 with no Curve25519, TPM 2.0 mandates P-256. The hardware composition is therefore the **P-256 hybrid variant** ([](/design/device-enrollment/)). Devices without a usable element (TPM 1.2, none) use the software keystore. RSA is rejected.
- **The algorithm travels in the key encoding.** A hybrid public key is `classical || ml_dsa_65`: the ML-DSA-65 half is a fixed 1,952-byte suffix and the classical half is 32 bytes (Ed25519) or 33 bytes (compressed SEC1 P-256), and the length selects the algorithm (`capsule-core/src/crypto/keys/hybrid_sig.rs`). A verifier walking a directory therefore knows which curve a `dsk_public` uses with no extra field. The closed set of two lengths is normative: adding a third classical algorithm is a new `crypto_suite_id` that introduces an explicit algorithm tag, never a third length.
- **The MLS leaf key is software.** The pinned MLS suite signs LeafNodes with Ed25519, which no secure element offers, so a device's MLS leaf signature key is a software Ed25519 key sealed under hardware-protected material and bound to the DSK by the hybrid leaf binding ([](/design/cryptography/mls/)). Hardware binding covers manifests and directory signatures; MLS commits are authenticated by the software leaf key. A device whose sandbox is read can sign group operations as that device until it is removed.
- **Revocation** is an IK-signed revocation statement plus an MLS `Remove` of the device's leaves. The server stops delivering wraps to it and the remaining devices rotate the group keys it held. The directory entry is **retained** with `revoked_at`, never deleted, so its pre-revocation signatures stay verifiable ([](/design/cryptography/provenance/#what-an-attacker-with-all-current-keys-still-cannot-do)).

### Owner Group Keys (OGKs)

**Deferred post-v1.** Nothing implements the owner MLS group; master-key escrow is the v1 recovery path. Also deferred: library-settings keying ([](/design/metadata/#the-library-settings-document)), OGK-escrow recovery and Drop-Key multi-device re-wrap.

- One MLS group per `owner_id` (its user set), with an AES-256 group root key.
- It escrows every album's AMK versions ([](#album-master-keys-amks)), never individual file keys, so any owner member can recover every asset independent of album membership.
- The epoch bumps on any owner-set change. Removal is an MLS `Remove`, after which the removed user gets no future OGK or AMK escrow.

### Album Master Keys (AMKs)

One MLS group per album, whose members are users with any role in the album roster ([](/design/authorization/#album-membership-on-the-server)). Secrecy (encryption) is separated from authorization (signatures):

| Material                                                                                                              | Comes from                                                                                             | Holders      | Use                                                                        |
| --------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | ------------ | -------------------------------------------------------------------------- |
| **AMK**: 32 B per epoch                                                                                               | The MLS exporter of the epoch ([](#where-an-amk-comes-from))                                           | All members  | Decrypt (read)                                                             |
| **Write-tier keypair**: hybrid Ed25519 + ML-DSA-65 ([](/design/cryptography/primitives/#signature-scheme)), per epoch | Minted at random by the epoch's committer; the public half attested in the commit's authenticated data | Writers only | Sign asset manifests ([](/design/cryptography/provenance/#asset-manifest)) |
| **Admin authority**                                                                                                   | The album owner's devices; no shared key in an MLS album                                               | The owner    | Commit membership and role changes, publish the roster                     |

The epoch bumps on member add or remove, role change, and scheduled rotation (e.g. 30 days). `amk_version` is the MLS epoch plus one.

#### Where an AMK Comes From

- **Exported, then stored.** When a member processes the commit that opens an epoch, it computes `AMK = MLS-Exporter("capsule amk v1", context = album_id, 32)` (RFC 9420 § 8.5) and writes it to the album keystore and the album escrow ([](/design/backup-recovery/#album-key-escrow)) before any blob is sealed under it. MLS deletes epoch secrets as it advances, so an AMK is never exported twice and never re-derived; the backup path reads stored AMKs only, which keeps it independent of the ratchet.
- **Why exported, not distributed.** Every member at the epoch computes the same AMK from the key schedule MLS already agreed and authenticated, so no member can hand another a different AMK for an epoch both hold, and a removed member never sees the secret. A distributed random key would need a sender-authority rule and a conflict rule; an exported one needs neither for current members.
- An album under the offline `ReferenceAuthority` has no group and no members to deliver to: its owner mints each AMK at random.

#### Key Delivery and Who May Send It

Key material a member cannot export itself rides MLS application messages ([](/design/cryptography/mls/#history-delivery-for-new-joiners)). MLS authenticates each message's sender leaf, and the receiver MUST check that leaf against the rule for the message kind:

| Message                 | Accepted only from                                                                             | Receiver check                                                                                                                                                                               |
| ----------------------- | ---------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AlbumKeyDistribution`  | Any member                                                                                     | A receipt for an epoch the receiver exported itself: the bytes MUST equal its own export. A mismatch is rejected and surfaced; the message carries no authority of its own.                  |
| `WriteTierDistribution` | The leaf that committed that epoch                                                             | The private half MUST derive the public half the commit attested. A joiner, which did not process that commit, takes the attestation from its Welcome, whose GroupInfo the committer signed. |
| `AlbumHistoryBundle`    | The leaf that committed the Add admitting the receiver (the signer of its Welcome's GroupInfo) | Entries strictly below the receiver's ceiling, within the album's history policy.                                                                                                            |

- **First valid delivery wins.** A second delivery for an epoch the receiver already holds, with different bytes, is rejected and surfaced as a key divergence; it never overwrites. Prior-epoch AMKs in a history bundle are also checked by the key commitment of the first blob opened under them ([](/design/cryptography/encryption/#reading-a-blob)), which turns a forged prior-epoch key into a _wrong key_, never a silent misread.
- **Writers only means sealed per writer.** The group channel is readable by every member, so the write-tier private half MUST NOT ride it in the clear whenever the writer set is smaller than the membership. The committer seals it to the leaf encryption key of each writer device at the new epoch with the labelled encryption of RFC 9420 §5.1.3, under the pinned suite's X-Wing HPKE: `EncryptWithLabel(leaf_encryption_key, "capsule write-tier v1", album_id || amk_version, write_tier_private)`. The HPKE `info` is therefore the serialized `EncryptContext` with label `"MLS 1.0 capsule write-tier v1"`, which no MLS-defined label shares, so a sealed write-tier key cannot be replayed as a path secret or a Welcome secret, nor the reverse. The receiver opens it with `DecryptWithLabel` under the same label and context. The sealed set travels as one application message. The writer set is the roster's writers at that epoch.
- **Only an owner or a writer commits.** Whoever commits an epoch mints its write-tier key, so a reader that commits would hold write capability. Members MUST reject a commit unless its committer leaf is bound ([](/design/cryptography/mls/)) to the album owner, or to a writer whose commit only adds or removes leaves of the writer's own account or updates its own leaf. Role changes, and adds or removes of other users, are owner-only. A reader's own device change is a proposal that an owner or writer commits.
- **Roles are group state.** The owner's commits carry the role map in a group-context extension, which the server's owner-signed roster mirrors, so members check committers against MLS state and never against the server.
- Under `ReferenceAuthority` the admin authority is an **admin-tier keypair** that signs the epoch ledger. It is held only by the owner's devices, carried in the owner's album escrow, and never delivered over MLS.

## Write Authorization

A DSK signature proves _which device_ produced an asset, not that it was _authorized to write_ at that epoch. The server is not trusted for authorization, since it may replay, reorder or surface reader-signed or removed-writer assets, and bugs must fail closed.

`verify_asset(manifest, ciphertext_hash, authority)` in `capsule-core/src/crypto/verify_asset.rs` is the **single chokepoint** by which a client acknowledges an asset on import, sync, federation, peering and restore. Rules:

- **Epoch-bound write proof.** Every manifest carries a DSK signature **and** a write-tier signature for its `amk_version` epoch, verified against the write-tier public key the authority ([](#write-authority-interface)) attests for that epoch. A server assertion is never sufficient.
- **Epoch ceiling is MLS-attested.** The client's monotonic `amk_version` ceiling comes from the admin-signed MLS commit chain ([](/design/cryptography/mls/#membership-operations)), never from the server's counter. A new client learns it from its Welcome ([](/design/cryptography/mls/#history-delivery-for-new-joiners)). The server's own monotonicity check ([](/design/threat-model/validation/#server-side-validation-invariants)) is only a backstop against clients skipping epochs.
- **Accepts and rejects.** An asset written by a writer later removed is accepted, because it was valid when written. One signed at an epoch where the signer was not a writer is rejected.
- **Backdating buys nothing.** Authorization and ordering ride the epoch and provenance chain. A removed writer holds no write-tier key for any later epoch, and old epochs fail the monotonic `amk_version` and chain-head checks.
- **Downgrade-resistant.** Both signatures cover `crypto_suite_id`, `protocol_version` and `prior_provenance_hash`.
- **Hash, not bytes.** It takes the ciphertext hash computed by the streaming path and never buffers a file.
- **Three outcomes:**
  - _accept_;
  - _terminal-reject_ (reader-signed, removed-writer, wrong-epoch, forged chain, suite-downgrade, tampered field): quarantined and surfaced in the audit trail ([](/design/cryptography/provenance/#provenance-of-library-modifications)), never silently dropped;
  - _pending_: `amk_version` is within the attested range but the AMK is not yet held, because the commit has not been processed or the key delivery has not arrived ([](/design/cryptography/mls/#history-delivery-for-new-joiners)). Held and retried, escalating to quarantine after a timeout.
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
- **Server identity anchor.** The document carries the IK's signature but not the IK, so publish sends the IK public key beside it in `X-Capsule-Identity-Key`. A password account's anchor is pinned at registration, which carries the IK public key and a proof of possession; an OIDC account's is pinned by its first publish ([](/design/authentication/#the-identity-anchor)). The first publish pins the account's recovery verification key beside it ([](#identity-rotation)). Every later publish MUST present the same IK and verify under it, else `403` with no detail about the stored anchor. The anchor changes only by a recovery-signed identity rotation. Global sign-out ([](/design/authentication/#explicit-revocation)) depends on this anchor.
- **Recovery key in the directory.** The signed core carries the recovery verification public key, so a peer pins it on first contact and can follow a later identity rotation without a trust-on-first-use reset. A peer follows a change of the recovery key only under the rule of [](#identity-rotation).
- **Field presence is signature-visible.** Optional fields (e.g. `dek_public`) encode as an **absent key**, never present-`null`, so directories signed before a field existed still verify. These are the same wire-presence rules as for manifests ([](/design/cryptography/provenance/#asset-manifest)).
- **Checked against the directory at receipt.** The server admits a manifest only if its signing device is listed and not revoked in the directory version in force when the server receives it (invariant 7 of [](/design/threat-model/validation/#server-side-validation-invariants)). It compares no `timestamp` with `added_at`: a self-asserted time cannot prove when a device was enrolled, so a backdated one buys nothing.
- Adding a user to an album adds all their non-revoked listed devices. MLS orders concurrent commits and the loser re-proposes.

## Identity-Based Key Derivation

How the master key and identity are held depends on the account type ([](/design/authentication/#account-types)).

### Registered accounts

- The master key is generated client-side and the server never holds it in the clear. Each device stores it wrapped under its DEK.
- A new device obtains it by cross-device recovery ([](/design/backup-recovery/#recovery-mechanisms)) or by unwrapping the server-side escrow ([](/design/backup-recovery/#master-key-escrow)) with the recovery passphrase.
- The IK is stored **sealed under the master key** (domain-separated) beside the escrow, at `/v1/auth/escrow/identity`, together with the default album id ([](#key-chain)). There is one per account, replaced atomically and independent of the escrow: recovery-secret rotation leaves it untouched.
- **Every enrolled device holds the account root.** A device stores the master key and so can open the sealed IK; enrolling another device needs exactly that ([](/design/device-enrollment/)). A compromised enrolled device is therefore a compromised **account** (master key, IK, every AMK), not a compromised DSK. The remedy is [](#master-key-compromise), and its IK step is possible only because the recovery secret is the one secret a device never keeps.
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

- **Reading** ([](/design/share-links/)): a link carries **file keys, never an AMK**, one per asset it covers ([](/design/share-links/#security-contract)). They are encapsulated under the link secret, optionally plus a password-based KDF layer ([](/design/cryptography/primitives/#password-based-kdf)).
- **Writing into an album: not supported.** There is no MLS membership and no write-tier key, so no valid manifest.
- **Contributing a drop** ([](/design/web-upload/)): the provisioning client mints a per-link **Drop Key** (hybrid X25519 + ML-KEM-768, as a DEK; see [](#device-keys)). The public half goes in the URL fragment. The private half is wrapped under the provisioner's master key (and OGK once it exists). Guests encapsulate a random per-asset key to it. A trusted client adopts the drop ([](/design/web-upload/#drop-and-adoption-lifecycle)), re-wrapping that key under the AMK (wrapped mode, [](#key-chain)). Deleting the link revokes the Drop Key.

## Key Rotation and Revocation

- **Master key:** rotation re-wraps device wraps, the AMK escrow and the sealed identity with its stored default album id ([](#registered-accounts)), then discards the old key. Signed-in sessions are unaffected.
- **Identity:** a recovery-signed rotation ([](#identity-rotation)).
- **Device:** revocation certificate ([](#device-keys)) + MLS `Remove` ([](/design/cryptography/mls/#membership-operations)).
- **Album member:** MLS `Remove` + AMK epoch bump.

### Identity Rotation

The recovery secret ([](/design/backup-recovery/#master-key-escrow)) is shown once and stored by no device, so it is the root an attacker holding a device lacks. It roots a **recovery verification key**:

```text
seed         = HKDF-SHA512(ikm = Argon2id(recovery_secret, recovery_salt), salt = user_id, info = "recovery-anchor/v1", L = 64)
recovery_key = hybrid Ed25519 + ML-DSA-65 signing key from seed
```

- `recovery_salt` and the Argon2id parameters (within [](/design/cryptography/primitives/#password-based-kdf)) are stored with the anchor. The salt differs from the escrow's, so the escrow wrap key and the recovery key never share an Argon2id output. The private key exists only while the user enters the secret.
- **IK rotation:** an `IdentityRotation { user_id, old_ik_public, new_ik_public, directory_version }` signed by the recovery key. The server replaces the anchor's IK only on a valid one, and peers that pinned the recovery key accept the new IK and show the change rather than raising a first-contact warning.
- **Recovery-key change** (a new recovery secret): signed by the current recovery key, it takes effect at once. Signed by the IK alone, because the user lost the secret but holds a device, it takes effect only after a **7-day time lock** announced on every device and cancellable by the current recovery key. The escrow is re-wrapped under the same rule, since escrow and recovery key come from one secret. A device-only attacker can start a change but cannot finish one the user sees.
- **Unconfirmed setup is the one exception.** The first directory publish pins the recovery key, but the user has not yet typed the secret back ([](/design/device-enrollment/#unconfirmed-setup)). Until the server records that confirmation, a resumed setup replaces the recovery key and the escrow under the IK at once, without the time lock. Confirmation is one-way.
- **Peers follow only a recovery-signed change.** A peer accepts a new recovery key only when the currently pinned recovery key signs the change. A change the IK alone signs, the time-locked path included, is not followed silently: the peer raises a warning of first-contact grade and keeps the old pin until the user re-verifies by safety numbers. The server's time lock protects the account; it gives a peer no evidence that the user, rather than a device thief, made the change.
- **Residual exposure.** The recovery public key lets its holder test a guess at the recovery secret offline, at one Argon2id evaluation per guess. The server holds the escrow blob, which permits the same test, and the directory gives the key to every peer. Neither matters, because offline guessing at Argon2id cost against a secret of at least 128 bits is infeasible.
- This is not a second root: the recovery secret is still the only user-held secret class ([](/design/backup-recovery/#single-root-invariant)), and the recovery key is a public commitment to it.

### Master-Key Compromise

The hierarchy wraps downward and is never inverted: wrapping AMKs under disposable device keys would forfeit recovery. Holding the master key opens the sealed IK, so master-key compromise and enrolled-device compromise are one event. Response, from a device the user trusts and with the recovery secret:

1. Rotate the IK ([](#identity-rotation)) and publish a directory under it that revokes every device not re-enrolled since the compromise.
2. Rotate the master key, re-sealing the identity and re-wrapping the escrow.
3. Re-key every album the account belongs to. The epoch bump yields fresh AMKs and write-tier keys ([](/design/mls-resilience/#group-re-keying-ceremony)).

Ciphertext already exfiltrated under old AMKs stays readable to the attacker. Records the attacker's devices signed before their revocation stay in the provenance chain under those devices, which is what makes them visible. Forward writes are clean. A user who has also lost the recovery secret can only reset the account.

## Project Release Keys

The weight registry ([](/design/ai/#weight-registry)) is the only artefact Capsule signs as a project rather than as a user. These keys are generated offline by the maintainer, not by a client. A two-level hierarchy means a routine key change never needs a client release. Both levels are hybrid Ed25519 + ML-DSA-65, because a registry record is long-lived evidence ([](/design/cryptography/primitives/#signature-scheme)).

| Key                                     | Held by                                                                 | Signs                                            | Rotated by                                                  |
| --------------------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------ | ----------------------------------------------------------- |
| **Release root** (active and successor) | The maintainer, offline on hardware tokens, with sealed offline backups | `delegate` and `checkpoint` records only         | A `delegate` signed by the successor, then a client release |
| **Registry key**                        | The maintainer, on a hardware token used to publish. Never a CI secret  | Release records (`release`, `supersede`, `yank`) | A new `delegate`                                            |

Validity is decided by **log position**, never by a clock: a record's `released_at` is self-asserted, and a timestamp is never used for authorization ([](/design/threat-model/schema-rules/#timestamp-grammar)). Every record's signature covers its `seq` and the hash of the record before it. A record therefore cannot be replayed at another position or into another log.

- **Compiled-in trust.** Every client compiles in two root public keys: the **active** root and a **pre-committed successor**. Both private halves are generated at one offline ceremony. The successor can retire the active root, so it is the stronger key. Its token and backup are therefore held apart from the active root's, and it is never used until a rotation or compromise.
- **Delegation.** A root-signed `delegate` record names one registry key and `max_seq`, the last log position it may sign. A release record at position `n` verifies only under the most recent `delegate` before `n`, and only if `n ≤ max_seq`. So a stolen registry key can sign at most up to `max_seq`. The maintainer publishes a fresh `delegate` before a delegation is exhausted.
- **Registry-key rotation.** A new `delegate` needs no client release. Records the old key signed before it stay valid, because their position fixes which delegation covered them. A later record signed by the old key is refused.
- **Root rotation.** The successor signs a `delegate`. Every client already trusts the successor, so from that position on every client refuses the old root, updated or not. A client that has seen the retiring `delegate` records the retirement locally. The retirement is **sticky**: no fetched log, fork or `checkpoint` reverses it. A client release then compiles in the successor as active, together with a newly generated successor. Delegations the old root signed earlier stay valid.
- **Root compromise.** If the active root is compromised, the same procedure runs at once, and it takes effect with no client release. If the successor is compromised, its holder can retire the active root, and only a client release that replaces both keys recovers. This is the cost of a pre-committed successor.
- **Registry-key compromise.** The root signs a superseding `delegate`. Records the compromised key published before that are withdrawn with `yank` records, never by rewriting the log.
- **Fork recovery.** A compromised key can serve some clients a signed fork that diverges from the canonical log. Those clients would otherwise refuse the canonical log forever, because a fetch must extend the cached copy. A root-signed `checkpoint` record names a log position and its hash. A client accepts a fetched log that diverges from its cache only if all three hold:
  - a `checkpoint` in the fetched log covers the common prefix;
  - a later `delegate` in the fetched log supersedes the key that signed every cached record past the divergence;
  - neither record is signed by a root the client no longer trusts.

  Fork recovery never discards a root-signed record, so the divergence must lie after every root-signed record in the cache. Otherwise the fetched log is refused. The client discards the forked suffix and its AI outputs, and regenerates them through [](/design/ai/#generation-cutover). Without both root-signed records, a divergent log is refused as a rollback.

## Failure Modes

Paths are the numbered [](/design/backup-recovery/#recovery-paths). Rejected manifests and distribution lag are the `verify_asset` outcomes above ([](#write-authorization)). Master-key loss is in [](/design/backup-recovery/#failure-modes).

| Failure                               | Detected / contained by                                                                   | Recovery                                                                                                                |
| ------------------------------------- | ----------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Device key loss                       | Device keys are disposable                                                                | Re-bootstrap (path 1 or 2); never recovered                                                                             |
| AMK loss                              | —                                                                                         | Path 3 or 4                                                                                                             |
| Write-tier key loss                   | —                                                                                         | Re-minted next epoch                                                                                                    |
| Master key compromise                 | —                                                                                         | Rotate and re-key ([](#master-key-compromise))                                                                          |
| Device compromise                     | —                                                                                         | Account compromise: [](#master-key-compromise). A device's key material includes the master key and, through it, the IK |
| Forged or conflicting key delivery    | Sender rule and export comparison ([](#key-delivery-and-who-may-send-it)); key commitment | Rejected and surfaced; the first valid delivery stands                                                                  |
| Hostile Argon2id parameters           | Bounds ([](/design/cryptography/primitives/#password-based-kdf))                          | Refused before any work                                                                                                 |
| AMK / write-tier compromise           | —                                                                                         | Epoch bump; an old epoch cannot read or sign future ones                                                                |
| Server compromise                     | Server never trusted                                                                      | Authorization from MLS history; data is end-to-end encrypted                                                            |
| One half of a hybrid primitive broken | Hybrid construction                                                                       | The other half holds                                                                                                    |
| Registry key compromise               | `max_seq` bounds what it can sign                                                         | Superseding `delegate` + `yank`; `checkpoint` recovers forked clients ([](#project-release-keys))                       |
| Active release root compromise        | —                                                                                         | Successor-signed `delegate` retires it with no client release ([](#project-release-keys))                               |
| Successor root compromise             | Separate custody                                                                          | Client release replacing both roots ([](#project-release-keys))                                                         |

## Known Gaps

- Key-delivery sender checks are not enforced: `OpenMlsAuthority::process_key_delivery` applies any member's message, and a joiner adopts the first write-tier key or history entry it sees (`capsule-core/src/crypto/authority/openmls_authority/mod.rs`).
- The write-tier private half rides the group channel to every member, because the writer filter returns all members.
- Core has no roles model, so neither the committer-role check nor the role map in the group context exists.
- Recovery-rooted identity rotation, the recovery key in the anchor and the directory, and the stored default album id are not implemented.
- Production albums use `ReferenceAuthority`; `OpenMlsAuthority` is not wired into the library.

## Validation

- Every failure-mode row tests its detection and recovery.
- **Derivation determinism:** HKDF output is byte-identical across platforms; RFC 5869 vectors.
- **Hardware round-trip:** per platform, generate a DSK in hardware, sign and verify. Reading private bytes MUST fail. The desktop leg is Windows TBS.
- **Directory field wire-absence:** unset optional fields leave the signed core byte-identical to a pre-field fixture. Set fields are signature-covered and tampering fails.
- **Rotation ceremony:** rotate against a real escrow. Every device wrap and AMK escrow entry is still recoverable.
- **`verify_asset` negatives:** reader-signed, removed-writer, wrong-epoch, forged chain, replay, suite-downgrade, half-valid hybrid `write_sig` ([](/design/cryptography/provenance/)).
- **`verify_asset` pending:** an unheld AMK in range is pending, delivering it accepts, and a version beyond the ceiling is terminal-reject.
- **Directory monotonicity:** `N-1` is refused and surfaced; `N+1` is accepted and pinned.
- **Release-key delegation:**
  - a record at or below its delegation's `max_seq` verifies, and one above it is refused;
  - once a successor-signed `delegate` exists, an old-root signature after it is refused, and one before it stays valid;
  - a `delegate` signed by neither compiled-in root is refused;
  - a divergent log is accepted only with a covering `checkpoint` and a superseding `delegate`, and is refused as a rollback otherwise;
  - a fork that would discard a cached root-signed record, or that is signed by a locally retired root, is refused;
  - a record replayed at a different `seq` or after a different prior hash fails its signature.
- **AMK export:** two members at one epoch export identical AMKs; a delivery with different bytes is rejected; the exported AMK is in the escrow before the first seal under it.
- **Sender rule:** a `WriteTierDistribution` or history bundle from a non-committer leaf is rejected, including one that reaches a joiner first.
- **Writer-only delivery:** with a reader in the group, no application message a reader can open contains a write-tier private half.
- **Committer roles:** a reader's commit, and a writer's commit that adds another user, are rejected by every member.
- **Identity rotation:** a recovery-signed rotation replaces the anchor and a pinned peer follows it; an IK-only recovery-key change does not take effect before its time lock and is cancelled by the recovery key.
- **Default album after rotation:** a master-key rotation followed by recovery resolves the same default album.
