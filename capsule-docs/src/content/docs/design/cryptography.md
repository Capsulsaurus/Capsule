---
title: Cryptography
description: Details of the key cryptography primitives for building Capsule on
---

## Pillars of Cryptography

*These are key aspects for those out of the loop.*

| Pillar              | The Core Question              | Primary Cryptographic / Security Tool       |
| ------------------- | ------------------------------ | ------------------------------------------- |
| **Confidentiality** | Can anyone else read this?     | Symmetric/Asymmetric Encryption (AES, RSA)  |
| **Integrity**       | Has this been tampered with?   | Hashing (SHA-256), MACs                     |
| **Availability**    | Can I access this right now?   | Redundancy, Backups, DDoS Protection        |
| **Authentication**  | Are you who you say you are?   | Digital Certificates, Passwords, Biometrics |
| **Authorization**   | Are you allowed to do this?    | Access Tokens, RBAC, ACLs                   |
| **Non-repudiation** | Can you deny doing this later? | Digital Signatures, Secure Audit Logs       |

## E2E Security Model

E2E security model has been prevalent for the past decade but applying the same restrictions on an asset-heavy application that aims to be performant and robust is not as trivial. This document outlines the high-level details of balancing security and capability trade-offs.

We need to encrypt assets (data) along with their metadata in a way that respects the hierarchy of accounts, albums, assets, and permissions. Think of them in layers:

- Identity: see [Signature Scheme](#signature-scheme) per device, cross-signed by the user master identity. See [Key Management](#key-management) for details.
- Group membership: One MLS group per shared album; each device is a leaf. See [Group Membership](#group-membership) for details.
- Asset encryption: [bulk AEAD](#bulk-aead) per file, keyed via the [KDF](#key-derivation) from per-album keys. See [Authenticated Asset Encryption](#authenticated-asset-encryption) for details.
- CBOR Metadata encryption: [bulk AEAD](#bulk-aead) per metadata blob, keyed via the [KDF](#key-derivation) from per-album keys. (We do not have a STREAM construction since it's typically fetched all together.) See [Metadata Encryption](#metadata-encryption) for details.

## Primitives Inventory

This table is **the single source of truth** for every cryptographic primitive Capsule
uses. Other docs (and the rest of this doc) reference these by anchor — they never
restate the choice. Swapping a primitive is a single-row edit here plus its dedicated
section below.

| Primitive                                 | Choice                                                   | Used for                                               |
| ----------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------ |
| [Cryptographic hash](#cryptographic-hash) | SHA-256                                                  | Content addressing, integrity verification             |
| [Key derivation (KDF)](#key-derivation)   | HKDF-SHA512                                              | Per-file and per-album key derivation                  |
| [Password-based KDF](#password-based-kdf) | Argon2id (device-tier-aware parameters)                  | Master-key escrow unwrap, backup unwrap                |
| [Bulk AEAD](#bulk-aead)                   | AES-256-GCM with [STREAM](#stream-construction)          | Asset and metadata ciphertext                          |
| [MLS control AEAD](#mls-control-aead)     | ChaCha20-Poly1305                                        | Inherited from the [MLS ciphersuite](#mls-ciphersuite) |
| [Signature scheme](#signature-scheme)     | Hybrid Ed25519 + ML-DSA-65                               | Identity, device, asset manifest, write tier           |
| [KEM](#kem)                               | X-Wing (X25519 + ML-KEM-768)                             | MLS HPKE                                               |
| [MLS ciphersuite](#mls-ciphersuite)       | `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (0x004D) | Group key management                                   |
| [Randomness](#randomness)                 | OS CSPRNG (`getrandom`)                                  | All keys, salts, nonces                                |
| [Transport](#transport-security)          | TLS 1.3 with hybrid X25519+ML-KEM                        | Client-server, server-server                           |

The per-primitive sections below carry the rationale; the table is the at-a-glance
reference.

## Versioning Identifiers

A faulty, malicious, or version-mismatched client could damage data by writing
under a primitive set the receiving side does not implement (see
[Threat Model](/design/threat-model/)). Three identifiers — owned here, in
[Versioning](/design/versioning/), and in [Metadata](/design/metadata/) — bind
each on-disk and on-wire structure to a specific set of primitives or schema so
that mismatches **fail closed** rather than corrupting state:

| Identifier         | Type                | Declared in                                                      | Carried in                                                                                                                                                |
| ------------------ | ------------------- | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crypto_suite_id`  | `u16`               | this doc                                                         | every [AssetManifest](#provenance-and-signed-manifest), every [metadata blob](#metadata-encryption), the backup [MANIFEST.cbor](/design/backup-recovery/) |
| `protocol_version` | string `YYYY-MM-DD` | [Versioning](/design/versioning/)                                | every AssetManifest, every wire request (see [Threat Model — Protocol Handshake](/design/threat-model/)), the album's MLS pin                             |
| `sidecar_schema`   | `u16`               | [Metadata — Sidecar Schema](/design/metadata/#sidecar-schema-v1) | CBOR sidecar field 0 (readable before parsing the rest)                                                                                                   |

`crypto_suite_id = 0x0001` denotes exactly the [Primitives Inventory](#primitives-inventory) above. Retiring any primitive (a broken SHA-256, a deprecated AEAD) **does not edit the row** — it adds a new row and a new suite id. An old AssetManifest carrying `0x0001` keeps verifying against the original row forever; new writes use the new suite id. This is the single-doc edit the inventory promises, generalized to the bundle.

The signatures on the manifest cover `crypto_suite_id` and `protocol_version`, so a downgrade-attempt (re-signing an existing manifest under a weaker suite) cannot be silently produced.

## Key Cryptographic Primitives

### Cryptographic Hash

We use SHA-256 (SHA-2) for content hashing, addressing, and integrity verification — everywhere, with no second hash algorithm. It is the most prevalent, audited, NIST-approved standard, and is hardware-accelerated on most modern platforms.

- Using exactly one hash means one less algorithm and implementation to maintain and audit.
- We reuse SHA-256 values across layers rather than recomputing them: the ciphertext hash used for content-addressing (see [Authenticated Asset Encryption](#authenticated-asset-encryption)) is the same value the [signed manifest](#provenance-and-signed-manifest) commits to, and the same value the upload protocol declares and verifies.
- SHA-3 was rejected for weaker hardware support; BLAKE3's parallelism is attractive but unneeded given simultaneous uploads, and its keyed mode is redundant against our already-authenticated encryption.

### Key Derivation

We use **HKDF-SHA512** for per-file and per-album key derivation. The wider 512-bit hash matches the post-quantum posture of the rest of the stack: under Grover's algorithm a 256-bit hash collapses to ~128-bit PQ security, while SHA-512 retains ~256-bit. KDFs are not on the hot path, so the cost difference is negligible. SHA-256 stays for *content addressing* — a different security goal where universal hardware acceleration matters more than PQ margin.

Every derivation includes a versioned `info` string (e.g. `"asset-file/v1"`, `"albums/v1"`) and a scope-unique salt (e.g. `album_id`, `file_id`) so a future KDF change can land alongside v1 derivations without a flag day.

### Password-based KDF

For password-based key derivation we use **Argon2id** with device-tier-aware parameters. Password-based derivation only runs at account recovery and device bootstrap — never on a hot path — so the cost is acceptable even on constrained hardware. Parameters are recorded inside the wrapped-blob [construction](#versioning) so they can be raised later without a flag day.

| Device tier             | Memory  | Iterations (`t`) | Parallelism (`p`) | When applies                             |
| ----------------------- | ------- | ---------------- | ----------------- | ---------------------------------------- |
| Low-RAM (≤ 2 GiB total) | 128 MiB | 3                | 1                 | Entry-level Android, low-end embedded    |
| Normal mobile / laptop  | 256 MiB | 3                | 1                 | Default for phones and laptops           |
| Desktop (≥ 8 GiB)       | 512 MiB | 4                | 1                 | Wrapping new escrow blobs from a desktop |

The salt is always a 32-byte CSPRNG draw. The tier chosen at *wrap* time is recorded
in the blob; *unwrap* respects whatever tier was recorded, so a desktop-wrapped blob
unwraps correctly on a phone (slowly) and vice versa.

### Bulk AEAD

For bulk data and metadata encryption we use **AES-256-GCM**. Combined with the [STREAM construction](#stream-construction) it covers asset ciphertext; standalone AES-256-GCM (fresh random nonce per blob) covers CBOR metadata blobs.

- AES hardware acceleration (Intel AES-NI, ARMv8 AES extensions, Apple Silicon dedicated AES units) is universal on every platform Capsule targets, so AEAD is never the bottleneck.
- We standardize on AES-GCM rather than ChaCha20-Poly1305 for stack consistency with the [SHA-2 family](#cryptographic-hash) and to keep one bulk-AEAD choice across the codebase. MLS retains ChaCha20-Poly1305 from its [ciphersuite spec](#mls-ciphersuite); that's a separate layer.
- Nonce misuse is the structural risk of GCM. We close it two ways: every file uses a freshly-derived per-file key (so the STREAM counter can safely start at zero), and standalone metadata blobs each draw a fresh CSPRNG nonce.

### MLS Control AEAD

For MLS control traffic we use **ChaCha20-Poly1305**, inherited from the [MLS ciphersuite](#mls-ciphersuite). This protects MLS's own membership and key messages, not user data; user data uses the [bulk AEAD](#bulk-aead) above.

### Signature Scheme

We use **hybrid Ed25519 + ML-DSA-65** for identity, device, asset manifest, and write-tier signatures. Both halves must verify before a peer is accepted. The classical and post-quantum halves are independent, so neither algorithm being broken compromises authentication. MLS LeafNode signatures stay Ed25519-only (pinned by the ciphersuite); the ML-DSA half lives at the identity layer — see [Group Membership](#group-membership).

### KEM

We use **X-Wing (X25519 + ML-KEM-768)**. This is the KEM defined by the [MLS ciphersuite](#mls-ciphersuite) we adopt.

### MLS Ciphersuite

We use **`MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519`** (OpenMLS ciphersuite 0x004D) — MLS (RFC 9420) with the PQ ciphersuites from `draft-ietf-mls-pq-ciphersuites`. See [Group Membership](#group-membership) for how the ciphersuite's choices (X-Wing KEM, ChaCha20-Poly1305 control AEAD, SHA-256 hash, Ed25519 leaf sigs) interact with the identity layer.

### Randomness

All keys, salts, and nonces are drawn from the operating system CSPRNG (`getrandom`). We never seed our own PRNG.

Nonces are never hand-rolled. The [STREAM construction](#stream-construction) derives per-chunk nonces deterministically; standalone [bulk-AEAD](#bulk-aead) metadata blobs each receive a fresh random nonce.

## Key Management

Capsule's keys form a single hierarchy with one backed-up root:

- The **account master key** is the only key that is escrowed/backed up. It does not encrypt assets directly. Its job is to (1) wrap the per-device identity private keys and (2) anchor the encrypted backup that escrows album keys.
- **Device keys** are hardware-bound, non-exportable, and therefore disposable — a device is re-bootstrapped from the master key rather than recovered.
- **Album keys** (AMKs) are random per-epoch keys ledgered in MLS, escrowed both in the master-key backup and in the [Owner Group](#owner-group-keys-ogks).

The guiding rule is to **keep the backup path independent of the MLS ratchet** so that losing all devices, but holding the recovery passphrase, still restores every photo. Do not be like Matrix, where undecryptable content is a routine failure mode. See [Failure Modes and Recovery](#failure-modes-and-recovery).

### Key Generation

All key generation happens client-side, from the OS CSPRNG. We use a PQ-safe ("post-quantum") hybrid scheme throughout: classical + PQ primitives combined so that breaking either one alone does not break security.

#### User Identity Keys (User IKs)

User IKs are generated once per user ever, and live forever (or until account compromise). This is the root of trust and signs everything below it. It is always verified out-of-band or via safety numbers.

A User IK is a **hybrid Ed25519 + ML-DSA-65** signing keypair generated entirely on the client at account creation. The private halves are wrapped under the [account master key](#registered-accounts) and never leave the client in the clear; the public halves are published in the signed [device directory](#per-user-device-coordination).

It can be revoked for a global account reset (irreversible, non-recoverable nuclear operation). Revocation is published as a separate revocation certificate, hybrid-signed by the IK itself, to a well-known location so clients can check for it.

#### Device Keys

Using the [user IK](#user-identity-keys-user-iks), each device's keys are cross-signed into the [device directory](#per-user-device-coordination):

1. **DSK** (Device Signing Key): hybrid **Ed25519 + ML-DSA-65**.
2. **DEK** (Device Encryption Key): hybrid **X25519 + ML-KEM-768**.

Both are signed by the IK (hybrid signature). Device private keys are **generated inside and never leave hardware** — Secure Enclave (iOS), StrongBox/Keystore (Android), TPM (desktop) — and are non-exportable. Because they cannot be backed up, devices are treated as disposable: a lost device is simply removed and a new one re-bootstrapped from the master key.

A device key can be revoked without affecting the user's identity or other devices. This allows for per-device access control and recovery from lost devices without a full account reset. Revocation is done by signing a revocation statement with the IK and publishing it to a well-known location. The server then refuses to deliver new key wraps to that device, and remaining devices rotate any group keys the revoked device had access to.

#### Owner Group Keys (OGKs)

Since assets' `owner_id` maps to a set of users, treat each owner as an MLS group.

- **Type:** Symmetric AES-256 root key of an MLS group whose members are the owner's user set.
- **Purpose:** A recovery/escrow layer. The OGK does **not** wrap individual file keys. Instead, it escrows every album's [AMK versions](#album-master-keys-amks), so any current owner member can always recover every album key — and therefore every asset — independent of album membership. This avoids double-wrapping each file while still guaranteeing the owner never loses access.
- **Epoch:** Bumps on any owner-set change. Every member's client commits to MLS, producing a new OGK; the server stores the welcome/commit messages.
- **Revocation:** Remove a user from the owner set → MLS Remove proposal → new epoch → the removed user's device can no longer derive future OGKs and is dropped from future AMK escrow.

#### Album Master Keys (AMKs)

Each album is its own MLS group. Members = users with any permission on the album.

- **Type:** Random 32-byte symmetric key, minted per epoch. AMKs are *not* derived from MLS epoch state (which is complicated to handle at edge cases) — they are random keys distributed *over* MLS application messages and ledgered.

Capsule separates **secrecy** (enforced by encryption) from **authorization** (enforced by signatures). We use one content key plus two signing capabilities, to minimize keys which can be possibly leaked:

- **`AMK` — the content key.** Read access. MLS delivers it to *all* album members. Holding it lets you decrypt; not holding it means you cannot.
- **Write capability — a per-epoch write-tier signing keypair.** Distributed via MLS to writers only. Used to sign [asset manifests](#provenance-and-signed-manifest). It rotates with the AMK epoch, so a removed writer cannot sign for future epochs. This is authorization, not secrecy. See [Write Authorization](#write-authorization).
- **Admin capability — an admin-tier signing keypair.** Distributed to admins only; used to sign MLS membership commits.

Epoch bump triggers: member add/remove, permission change, scheduled rotation (e.g., every 30 days for long-lived albums).

#### Write Authorization

A device signature on an [asset manifest](#provenance-and-signed-manifest) proves *which device* produced an asset — but not that the device was *authorized to write* to that album at that time. The server is **not trusted for authorization**: it could replay, reorder, or surface an asset signed by a reader-only device, a removed writer, or a device acting outside its write window. A bug could also produce such an asset. Both must be rejected robustly, with the verification logic kept small enough to be hard to get wrong.

- **Epoch-bound write proof.** Every asset manifest carries, in addition to the device DSK signature, a signature under the album's **per-epoch write-tier signing key**. Only writers at that epoch hold that key. The manifest's `amk_version` identifies the epoch.
- **Authorization authority is MLS history, not the server.** The client verifies the write-tier signature against the write-tier public key it learned for that epoch *from MLS* — the album's MLS commit chain (admin-signed) is the sole authority on who could write when. A server-asserted authorization is never sufficient.
- **What this accepts vs. rejects.** An asset signed by a writer who was *later* removed is still acknowledged — it was valid when written, and nothing after removal un-seeds it. An asset signed at an epoch where the signer lacked write capability is **rejected**: an attacker (or a buggy/colluding server) cannot produce a valid write-tier signature for an epoch they were not a writer in.
- **Single verification chokepoint.** All of this lives in one `verify_asset(manifest, ciphertext, mls_state)` function in `capsule-core/crypto` — the only path by which a client acknowledges an asset. Per [contract-driven development](#implementation), it ships with exhaustive negative test cases: reader-signed, removed-writer, wrong-epoch, forged certificate chain, replayed manifest.
- **Defensive failure handling.** A verification failure is *never* silently dropped and *never* silently accepted. The asset is quarantined and surfaced in the [provenance/audit trail](#provenance-of-library-modifications) so an operator can distinguish a bug from an attack after the fact. This bounds the blast radius of an implementation bug.
- **Downgrade-resistant.** Both signatures cover `crypto_suite_id`, `protocol_version`, and `prior_provenance_hash`. A manifest cannot be silently re-signed under a weaker suite or back-dated onto a different chain position without breaking either signature; an attempt to do so is rejected at the same `verify_asset` chokepoint.
- **Timestamp grammar.** Servers refuse a manifest whose `timestamp` is outside **±30 days of server clock** (configurable). The cryptography proves "this asset was signed by a device that held epoch-N write capability"; the time window prevents a buggy or hostile client from injecting timestamps decades in the past or future that would silently distort the audit trail. The grammar lives in [Threat Model](/design/threat-model/) and is mirrored in [Server-Side Validation Invariants](/design/threat-model/).

#### Forward Secrecy & Post-Compromise Security

The MLS-based scheme provides forward secrecy (FS) and post-compromise security (PCS). The specific implementation we follow is MLS (RFC 9420) with the PQ ciphersuites from `draft-ietf-mls-pq-ciphersuites`.

**Clarification:** True FS on data-at-rest is a contradiction (the ciphertext persists). What MLS gives you at each epoch bump is: a compromise of the current epoch's keys doesn't help an attacker read past epochs, and removed members can't read future epochs. That's the practical security property you want.

For data-in-transit between clients and server (uploads, key-bundle fetches), use TLS 1.3 with ephemeral ECDHE — that's where session-level FS lives. See [Transport Security](#transport-security).

#### Resisting Key Loss

Loss of keys — and thus loss of data — is a first-class failure mode. The master key, not any MLS ratchet state, is the single backed-up root. All safeguards and the redundant restore paths are consolidated in [Failure Modes and Recovery](#failure-modes-and-recovery).

#### Key Chain

The account master key does **not** derive album keys — albums are MLS groups with random AMKs. The master key's role is to wrap device identity keys and to anchor the encrypted backup that escrows AMKs:

```plaintext
account_master_key (backed up — see Resisting Key Loss)
  ├─ wraps device identity private keys (IK / DSK / DEK private halves)
  └─ anchors the encrypted backup that escrows:
        AMK_v{n}  (random 32 bytes, per album, minted per MLS epoch)
          └─ HKDF-SHA512(ikm=AMK_v{n}, salt=file_id, info="asset-file/v1") → 32-byte AES file key
                └─ AES-256-GCM-STREAM
```

Important details on construction:

- Always include a version string in `info` so you can rotate the KDF later.
- Salt with something unique per scope (`album_id`, `file_id`) — don't reuse salts across scopes.
- The 512-bit KDF output is truncated to 32 bytes (256-bits) for the AES-256 file key. See [Key Derivation](#key-derivation) for the SHA-512 rationale.
- Each file gets a fresh derived key, so the STREAM nonce can safely start at zero per file.

Photo/media keys specifically: separate the "MLS/ratchet" world from "data at rest." Per-album AMKs are escrowed in the server-side encrypted backup (see [Backup and Recovery](/design/backup-recovery/)) and the [OGK](#owner-group-keys-ogks) — not derived from ratchet state — so losing all devices but holding the recovery passphrase still restores photos. Ratchet keys are expected to be ephemeral.

### Identity-based Key Derivation

Since all assets are encrypted via keys ultimately recoverable from an account's master key, we encapsulate user identity keys differently depending on the [account type](/design/authentication/#account-types).

#### Registered accounts

Most users have their own unique master key. It is **generated client-side** at account creation from the OS CSPRNG. The server never holds the naked master key. Each device stores its own copy wrapped under that device's DEK; a new device obtains the master key either via [cross-device recovery](/design/backup-recovery/#recovery-mechanisms) or by unwrapping the [encrypted server-side backup](/design/backup-recovery/#master-key-escrow) with the recovery passphrase.

#### Delegated/Sponsored accounts

A sponsored account is anchored under the sponsor's master key but holds its own encryption keys. The mechanism — and the only sound way to revoke — is:

1. **Per-sponsoree KEK.** When a sponsor creates a sponsored account, the sponsor draws a fresh 32-byte **sponsoree KEK** from the CSPRNG (it is *not* derived from the master key — a deterministic derivation would be reproducible by the sponsor at any future point, defeating revocation). The KEK is wrapped under the sponsor's master key and stored in the sponsor's escrowed hierarchy.
2. **Sponsoree key material.** The sponsoree's own identity, device, and album keys are generated normally (see the rest of this section). Their private halves are wrapped under the sponsoree KEK rather than directly under the sponsor's master key, so the sponsor can re-wrap or destroy a single sponsoree's keys without touching its own or the other sponsorees'.
3. **Shared-asset access.** Sponsorees gain access to a sponsor's shared albums via ordinary MLS membership (the sponsoree's devices are added as MLS leaves in the sponsor's album groups). The KEK is *not* a content key — it only wraps the sponsoree's private keys.
4. **Revocation.** Revocation is a three-step operation, all signed by the sponsor's IK:
   - **Rotate** the sponsoree KEK: draw a new KEK, re-wrap surviving sponsorees if any, drop the old KEK.
   - **Publish** an IK-signed revocation certificate naming the revoked sponsoree's identity and the timestamp.
   - **Remove** the revoked sponsoree's devices from every MLS group they were a member of (album groups, owner group) via the standard [MLS Remove](#membership-operations) flow, bumping AMK epochs.

The sponsor's *own* master key is untouched by any sponsoree revocation. The published revocation certificate is what clients and [federated](/design/federation/) peers check to refuse traffic from a revoked sponsoree.

#### Non-registered accounts

**Reading.** Since key management operates at the user level, userless share links are handled distinctly. We encapsulate the decryption keys around the secret stored in the link. The owner can optionally attach a password, in which case the [password-based KDF](#password-based-kdf) adds a second encapsulation layer on top of the link secret.

**Writing.** Writing is **not supported** for non-registered accounts. Every uploaded asset must be encrypted under an album key and signed with a write-tier key; a non-registered user has neither a device encryption key (DEK) nor a place in any album's MLS group, so it cannot produce a valid [asset manifest](#provenance-and-signed-manifest). Supporting guest uploads would require an ephemeral link-scoped key hierarchy; this is a deliberate non-goal to keep the design simple.

### Key Rotation and Revocation

- **Master key rotation.** The master key can be replaced at will. Rotation re-wraps the key hierarchy (device-key wraps and the AMK escrow blob) under the new master key; the old master key is retained only long enough to complete the re-wrap, then discarded. Existing signed-in sessions hold device and derived keys directly and are **unaffected** — they keep working through the rotation.
- **Device revocation.** Handled via the [device key](#device-keys) revocation certificate plus an MLS `Remove` for that device's leaves (see [Membership operations](#membership-operations)).
- **Album-member revocation.** Handled by an MLS `Remove` and an AMK epoch bump (see [Membership operations](#membership-operations)).

## Group Membership

Capsule's group layer is the [MLS ciphersuite](#mls-ciphersuite) from the inventory. The ciphersuite's choice of [ChaCha20-Poly1305](#mls-control-aead) (rather than [AES-GCM](#bulk-aead) used for user data) is acceptable because:

- It only protects MLS's own control messages (kilobytes of membership and key data, not your photos).
- ChaCha20-Poly1305 is one of the two most-audited AEADs in existence.
- The alternative is a classical-only MLS ciphersuite plus a hand-rolled PQ retrofit — exactly the custom crypto we're trying to avoid.

One follow-on: MLS binds LeafNode signatures to Ed25519 in this suite, so the ML-DSA half of the [hybrid signature scheme](#signature-scheme) lives at the **application layer** — identity certificates sign the Ed25519 MLS key with both Ed25519 and ML-DSA, and peers verify both before accepting a device into a group. This keeps MLS pure while preserving PQ authentication end-to-end.

### Membership operations

**Add user Bob to album:**

1. Fetch Bob's device directory (list of his devices with KeyPackages published to the server)
2. MLS `Add` proposal + `Commit` adding all Bob's devices as leaves
3. The `Welcome` message to Bob's devices carries current `AMK_v_current` as a Welcome extension
4. If full history is desired (usually yes for shared albums), also include `AMK_v1..AMK_{current-1}` in the Welcome — Bob's devices can now decrypt everything
5. If only post-join history, omit older AMKs — Bob sees only future photos

**Remove user Charlie:**

1. MLS `Remove` proposal + `Commit` removing all Charlie's devices
2. MLS advances to a new epoch; Charlie's devices can no longer read MLS traffic
3. Committer generates fresh `AMK_v{current+1}` and broadcasts via MLS to remaining members
4. All future photo uploads use `AMK_v{current+1}`
5. Charlie retains `AMK_v1..current` locally, so he can still decrypt photos he *already had access to* — this is correct behavior (he already had those photos; nothing you do after removal un-seeds them). But new uploads are invisible to him.

**Add new device for existing member:**

1. Alice's existing device adds Alice's new device as a leaf in the MLS group
2. Welcome carries all AMK versions Alice is entitled to
3. New device is now equivalent to Alice's other devices

**Remove lost device:**

1. Any of user's remaining devices issues MLS `Remove` for the lost device
2. Treat like a removal above — bump AMK version, since you must assume the lost device's keys are compromised

## Per-user device coordination

Each user publishes a signed device directory:

```rust
DeviceDirectory {
  user_id,
  devices: [
    { device_id, ed25519_pk, mldsa_pk, key_package_ref, added_at, signed_by_master },
    ...
  ],
  signature: Hybrid(master_ed25519, master_mldsa)
}
```

When Alice's device A1 adds Bob to an album, it fetches Bob's directory, verifies the hybrid signature against Bob's published master identity, and adds all Bob's listed devices. Alice's other devices (A2, A3) see the MLS commit and update local state — MLS handles idempotent application of commits, so this just works.

Conflicts (A1 and A2 trying to add different people simultaneously) are handled by MLS's proposal/commit ordering — one wins, the other re-proposes on top. OpenMLS exposes this.

### History delivery for new joiners

This is the one spot where you write real custom code. Two patterns:

**Full history (recommended for shared albums):**
Welcome message carries encrypted blob of `[AMK_v1, AMK_v2, ..., AMK_current]`. New joiner decrypts all, can now read every photo.

**Capped history (e.g., last 90 days):**
Only include AMKs corresponding to epochs ≥ threshold. Older photos remain visible but not decryptable — you show placeholders.

Matrix supports both; most photo-sharing products default to full history. Pick one default, expose the choice if needed later.

### Notes on Scaling

MLS scales to thousands of leaves, so even a 50-user album (200+ devices) is fine. Note that every `Commit` touches the whole tree and each `Welcome` carries `log(N)` path secrets plus the AMK blob — a cost to watch for very large shared albums.

## Authenticated Asset Encryption

Every asset is content-addressed by the SHA-256 of its ciphertext and encrypted with a unique file key. We use AES-256-GCM with the STREAM construction for authenticated encryption. The file key is derived from the appropriate [AMK](#album-master-keys-amks); the AMK itself is recoverable from the account's master key (see [Identity-based Key Derivation](#identity-based-key-derivation)).

### Asset Key Derivation

Each asset is encrypted with a key derived from a versioned album master key (AMK), distributed and ledgered over MLS (see [Group Membership](#group-membership)). Note we never derive a key from the MLS epoch's internal state.

An album's AMK ledger looks like this:

```rust
Album {
  id: UUID,
  mls_group: MlsGroup,
  keys: [
    AMK_v1: (random 32 bytes, created at album creation),
    AMK_v2: (random 32 bytes, created when member X was removed),
    AMK_v3: ...
  ],
  current_version: 3,
}
```

The per-file key is derived from the AMK version that encrypted it, using the [KDF](#key-derivation):

```rust
file_key = HKDF_SHA512(
  ikm: AMK_v{amk_version},
  salt: file_id,
  info: "asset-file/v1",
  length: 32        // 32 bytes for AES-256; HKDF-SHA512 expand truncates safely
)
```

AMKs are delivered over MLS application messages. When epoch N's MLS group is established, the creating device sends an `AlbumKeyDistribution { amk_version, amk_bytes }` message through MLS. Every current member's device receives and stores it locally (hardware-wrapped).

### Provenance and Signed Manifest

Capsule frequently needs a verifiable trace of *who* produced an asset, so the provenance signature must be cryptographically bound to the ciphertext — while still allowing streaming. We do this with a small **signed manifest** rather than a Merkle tree: the STREAM construction already detects per-chunk tampering, truncation, and reordering, so a Merkle tree's only marginal gain (early-abort on a forged *whole-file* signature) is not worth the extra format complexity.

Each asset is stored as:

```rust
AssetManifest {
  version:                "asset-manifest/v1",
  crypto_suite_id:        u16,            // see Versioning Identifiers above
  protocol_version:       String,         // YYYY-MM-DD; matches album pin
  file_id:                UUID,
  album_id:               UUID,
  amk_version:            u32,            // identifies the AMK epoch + write-tier key
  ciphertext_hash:        { algo: String, value: bytes },  // content address; reused by upload protocol
  plaintext_size:         u64,
  chunk_size:             u32,            // plaintext bytes per chunk (65,520)
  nonce_prefix:           [u8; 7],        // STREAM nonce prefix, random per file
  created_by_user:        UUID,
  created_by_device:      UUID,
  client_version:         String,
  timestamp:              RFC3339,        // bounded to ±30 days of server clock at accept
  action:                 enum,           // create | replace | delete | metadata-update
                                          //   | derivative-add | derivative-replace | trash-restore
  prior_provenance_hash:  Option<[u8;32]>, // SHA-256 over the previous manifest in this asset's
                                           // provenance chain. null only for `action = create`.
                                           // See Provenance of Library Modifications.

  device_sig:        Hybrid(Ed25519, ML-DSA-65),  // over all fields above
  write_sig:         Signature,                   // under epoch write-tier key, over all fields above
}

AssetBlob {
  manifest: AssetManifest,
  chunks:   [AES-256-GCM-STREAM encrypted chunks],
}
```

The manifest carries **two signatures**, and a client acknowledges the asset only if **both** verify:

1. `device_sig` — hybrid Ed25519 + ML-DSA-65 by the uploading device's [DSK](#device-keys). Provides provenance; the device certificate chains to the user IK via the [device directory](#per-user-device-coordination).
2. `write_sig` — a signature under the epoch's [write-tier key](#album-master-keys-amks). Proves the signer held write authorization at `amk_version` (see [Write Authorization](#write-authorization)).

The signed manifest is stored as the encrypted asset's header and is itself part of the [provenance record](#provenance-of-library-modifications). The same signing approach applies to other surfaces — [metadata blobs and sidecars](#metadata-encryption) and the [device directory](#per-user-device-coordination) are each hybrid, device-signed, and versioned.

**Streaming is preserved.** The STREAM authentication tags verify every chunk *during* the stream. The manifest signature is a one-time provenance check. `ciphertext_hash.value` is computed incrementally as bytes arrive and confirmed at stream end — no separate pass, no buffering the whole file.

### Encryption Workflow

Encrypting an asset for upload:

1. Derive `file_key` from `AMK_v{current}` (see [Asset Key Derivation](#asset-key-derivation)).
2. Generate a random 7-byte `nonce_prefix` from the OS CSPRNG.
3. Split the plaintext into 65,520-byte chunks and encrypt sequentially with `EncryptorBE32<Aes256Gcm>`, producing 64 KiB ciphertext chunks (16-byte tag each); the final chunk is flagged as last.
4. Compute `ciphertext_hash.value` incrementally over the produced ciphertext (the `algo` is fixed by `crypto_suite_id`).
5. Build and sign the [manifest](#provenance-and-signed-manifest) (device signature + write-tier signature).
6. Upload the blob (see [Import Synchronization](/design/import-synchronization/)).

Streaming download / ranged reads:

- **Sequential:** `DecryptorBE32<Aes256Gcm>` consumes chunks in order, verifying each tag.
- **Ranged:** To start at plaintext byte `B`, the client computes `chunk_index = B / 65,520`. Because the [STREAM construction](#stream-construction) derives each chunk's nonce deterministically, chunk `i` decrypts independently given `file_key` and `i` — the server need only serve that 64 KiB ciphertext chunk, which the client decrypts and verifies.

### STREAM Construction

Our scheme strictly requires streaming.

The chosen method is AES-256-GCM with the STREAM construction (Hoang-Reyhanitabar-Rogaway-Vizár, 2015). STREAM splits the file into chunks, encrypts each with AES-GCM using a structured nonce (`prefix || counter || last-chunk-flag`), and guarantees you detect truncation, reordering, and chunk deletion.

In Rust: the RustCrypto `aead` crate exposes `stream::EncryptorBE32<Aes256Gcm>` and `stream::DecryptorBE32<Aes256Gcm>` — drop-in. We use a 65,520-byte plaintext chunk → 64 KiB ciphertext chunk. (Note the upload transport's 4 KiB chunk alignment, described in [Import Synchronization](/design/import-synchronization/), is a separate concern from this crypto chunk size.)

## Metadata Encryption

Not all metadata can be encrypted — some must stay server-readable for routing and preview. The split is deliberate:

- **Encrypted** (AES-256-GCM under a key derived from the album's AMK, fresh random nonce per blob): the CBOR sidecar / metadata blobs. Each blob is independently versioned and signed like an [asset manifest](#provenance-and-signed-manifest).
- **Server-plaintext by necessity:** `owner_id`, the [ciphertext content hash](#primitives-inventory), the ciphertext size, the [chromahash LQIP](/design/thumbnails/#lqip), and `dominant_color`. These are needed for routing and for generating previews without decryption. This is a deliberate, documented trade-off.
- **AI embeddings** (semantic-search vectors, face embeddings) are sensitive — a user can be re-identified from them. They are kept plaintext *locally* (vector search requires it) but encrypted at rest in the server-side backup.

CBOR metadata blobs use **deterministic encoding** (RFC 8949 §4.2). Because a blob's hash is what content-addresses it and what the [signed manifest](#provenance-and-signed-manifest) commits to, two implementations encoding the same logical metadata must produce byte-identical output — otherwise the hash diverges and the signature fails to verify across [federated](/design/federation/) peers.

### Metadata Blob Wire Format

An encrypted metadata blob is a single contiguous byte string. Implementations MUST produce and consume exactly this layout, with no framing variations, so two correct implementations can compute identical content hashes byte-for-byte.

```text
+---------------------+---------------------+--------------------------+---------------+
| crypto_suite_id (2) | nonce (12 bytes)    | ciphertext (variable)    | tag (16 bytes)|
+---------------------+---------------------+--------------------------+---------------+
| big-endian u16      | fresh CSPRNG draw   | AES-256-GCM(plaintext)   | GCM tag       |
```

- `crypto_suite_id` (2 bytes, big-endian `u16`) — pins the AEAD and KDF used to derive the key. Identical to the field carried inside the manifest (see [Versioning Identifiers](#versioning-identifiers)), and a mismatch with the manifest's value rejects the blob at decode.
- `nonce` (12 bytes) — fresh OS-CSPRNG per blob; never reused, never derived.
- `ciphertext` — the deterministically-encoded CBOR plaintext, sealed with AES-256-GCM under `HKDF-SHA512(ikm=AMK_v{n}, salt=blob_id, info="metadata-blob/v1", length=32)`.
- `tag` (16 bytes) — GCM authentication tag.

The total blob's `ciphertext_hash` (in the asset's [signed manifest](#provenance-and-signed-manifest)) is computed over the full byte string above — header, nonce, ciphertext, and tag concatenated.

## Provenance of Library Modifications

Every modification of data or metadata produces a **provenance record** — timestamp, device, client version, action — anchored by a [signed manifest](#provenance-and-signed-manifest). The records form an **append-only, hash-chained log per asset**, which is what lets an operator distinguish a legitimate delete from a malicious or bug-induced one after the fact, and what defeats the [stale-revival attack](/design/threat-model/) described in the Threat Model.

### Chained, Append-Only Structure

```rust
ProvenanceRecord {
  asset_id:              UUID,
  manifest:              AssetManifest,           // see Provenance and Signed Manifest
  prior_provenance_hash: Option<[u8;32]>,         // SHA-256 over the previous record;
                                                  // null only for `action = create`
  // The manifest's own `prior_provenance_hash` mirrors this value, so signature
  // coverage of the manifest is signature coverage of the chain link itself.
}
```

Each non-create record references its predecessor by hash; a rewrite of any past record breaks the chain at that point and is detectable by any client walking forward from `create`.

### What an Attacker With All Current Keys Still Cannot Do

Even if every current key (every device's DSK, every album's current AMK and write-tier key) is compromised:

- **Forward writes are possible** — the attacker can append new records, just like any holder of those keys.
- **Past records cannot be rewritten** — the prior record was signed by a (possibly retired) device whose hybrid signature is still verifiable against the public half published in the [device directory](#per-user-device-coordination). Replacing the past record would require forging that earlier device's signature, which the hybrid construction prevents.
- **Past records cannot be silently removed** — every later record carries the prior hash, so a removal breaks the chain.

This bounds the blast radius of a credential compromise: history is read-only.

### Physical Storage

- **Client.** An append-only CBOR file at `media/{YYYY}/{YYYY-MM}/{uuid}.provenance.cbor`, alongside the asset and its sidecar. The file is a sequence of `ProvenanceRecord` entries. The client never deletes this file — on hard-delete of an asset the log persists as a tombstone-with-history.
- **Server.** A content-addressed encrypted blob, distinct from the [encrypted metadata blob](#metadata-encryption), so a metadata edit (which mints a new metadata blob) never rewrites history. The server's no-key envelope of every provenance write includes `prior_provenance_hash`, so the server can enforce monotonic chain advance without holding any key — see [Threat Model — Server-Side Validation Invariants](/design/threat-model/).

The server is **append-only** for provenance: there is no API path that overwrites or deletes an existing entry. An attempt is rejected at the [server's structural validation layer](/design/threat-model/).

### Derivative Provenance

Thumbnails, previews, and embeddings are generated client-side and uploaded as ordinary encrypted blobs. Without provenance they would be silently overwritable by any client with write capability — a buggy v4 client could quietly replace a v3 client's good thumbnail with a corrupt one. To prevent this, every derivative carries a small signed manifest of its own:

```rust
DerivativeManifest {
  version:               "derivative-manifest/v1",
  crypto_suite_id:       u16,
  source_asset_id:       UUID,
  role:                  enum,            // thumbnail | preview | lqip | embedding
  format:                String,          // e.g. "image/avif", "embedding/mobileclip-b"
  ciphertext_hash:       { algo, value },
  generated_by_device:   UUID,
  generated_by_client:   String,
  model_id:              Option<String>,  // for embeddings; see ML Models
  model_version:         Option<String>,  // for embeddings
  generated_at:          RFC3339,
  prior_provenance_hash: Option<[u8;32]>, // chained per (asset_id, role)
  device_sig:            Hybrid(Ed25519, ML-DSA-65),
  write_sig:             Signature,       // under the album's epoch write-tier key
}
```

A derivative overwrite is therefore a `derivative-replace` lifecycle action that appends to the provenance chain like any other write. Quarantine semantics from [Write Authorization](#write-authorization) apply: a derivative whose manifest fails verification is surfaced, never silently applied — a buggy client cannot poison a derivative under the receiving side's nose.

## Failure Modes and Recovery

Capsule treats loss of data — and loss of the keys that decrypt it — as a first-class concern. This section enumerates what can go wrong, how each failure is detected or contained, and the redundant, independent paths that restore a user's *entire* asset collection — including after catastrophic software bugs, not just key loss.

### Failure Mode Catalog

| Failure mode                                                                                         | Detected / contained by                                                                                                                                                    | Recovery path                                                                                                                        |
| ---------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| **Master key loss**                                                                                  | —                                                                                                                                                                          | Master-key escrow (path 1) or cross-device recovery (path 2)                                                                         |
| **Device key loss**                                                                                  | Device keys are disposable by design                                                                                                                                       | Re-bootstrap from the master key (path 1/2); device keys are never recovered                                                         |
| **AMK loss** (album key)                                                                             | —                                                                                                                                                                          | OGK escrow (path 3) and the master-key-anchored backup escrow (path 4)                                                               |
| **Write-tier key loss**                                                                              | —                                                                                                                                                                          | Re-minted and redistributed over MLS at the next epoch; no asset is lost                                                             |
| **Master key compromise**                                                                            | —                                                                                                                                                                          | Master-key rotation re-wraps the hierarchy — see [Key Rotation and Revocation](#key-rotation-and-revocation)                         |
| **Device compromise**                                                                                | —                                                                                                                                                                          | Device revocation certificate + MLS `Remove`; surviving devices rotate group keys                                                    |
| **AMK / write-tier compromise**                                                                      | —                                                                                                                                                                          | MLS epoch bump mints a fresh AMK and write-tier key; the compromised epoch cannot read or sign future epochs                         |
| **Server compromise**                                                                                | Server is never trusted for authorization or plaintext                                                                                                                     | Authorization is verified against MLS history; data is E2E-encrypted at rest                                                         |
| **Classical primitive broken** (Ed25519, X25519)                                                     | Hybrid construction                                                                                                                                                        | The PQ half (ML-DSA-65 / ML-KEM-768) still holds — confidentiality and authentication survive                                        |
| **PQ primitive broken** (ML-DSA, ML-KEM)                                                             | Hybrid construction                                                                                                                                                        | The classical half still holds                                                                                                       |
| **Ciphertext corruption; chunk truncation, reorder, or deletion**                                    | AES-256-GCM-STREAM per-chunk tags + `ciphertext_sha256`                                                                                                                    | Re-fetch the blob from a content-addressed copy (path 6)                                                                             |
| **Reader-signed / removed-writer / wrong-epoch / forged-chain / replayed manifest**                  | The single [`verify_asset`](#write-authorization) chokepoint                                                                                                               | Asset is quarantined and surfaced in the [audit trail](#provenance-of-library-modifications)                                         |
| **MLS ratchet corruption or loss**                                                                   | —                                                                                                                                                                          | The recovery path is independent of ratchet state (paths 1, 3, 4)                                                                    |
| **Backup incompleteness** (a referenced `amk_version` missing from the escrow)                       | Backup verification's AMK-completeness check                                                                                                                               | Caught before the backup is relied on; re-export                                                                                     |
| **Nonce reuse**                                                                                      | Structurally prevented                                                                                                                                                     | STREAM derives per-chunk nonces; metadata blobs draw fresh random nonces; a fresh per-file key lets the STREAM counter start at zero |
| **CBOR non-determinism** breaking cross-peer signature verification                                  | RFC 8949 §4.2 deterministic encoding                                                                                                                                       | Byte-identical re-encoding; the signature verifies                                                                                   |
| **Catastrophic software bug** corrupting the library DB / index                                      | The DB is a rebuildable cache, not a source of truth                                                                                                                       | Filesystem rebuild from CBOR sidecars (path 5)                                                                                       |
| **Erroneous delete** (bug or user)                                                                   | Soft-delete is the default                                                                                                                                                 | Restore from trash within the retention window (path 7)                                                                              |
| **Stale-revival attempt** (peer or restore sends an old-but-validly-signed manifest)                 | `prior_provenance_hash` chain (see [Provenance](#provenance-of-library-modifications)) and matching server-side envelope check (see [Threat Model](/design/threat-model/)) | Manifest is quarantined; chain advance is refused on both client and server                                                          |
| **Suite-downgrade attempt** (re-sign a manifest under a weaker `crypto_suite_id`)                    | Signature covers `crypto_suite_id` and `protocol_version`                                                                                                                  | Verification fails at `verify_asset`; manifest is quarantined                                                                        |
| **Derivative poisoning** (buggy or hostile client overwrites a good thumbnail/embedding)             | Every derivative carries a [`DerivativeManifest`](#derivative-provenance) on its own chain                                                                                 | Overwrite without a valid manifest is rejected; provenance chain detects an unauthorized replacement                                 |
| **Cross-schema sidecar overwrite** (old client writes back a sidecar after stripping unknown fields) | Sidecar signature covers every byte including unknown fields; old client `refuses to write` when `sidecar_schema` exceeds its max known                                    | Old client cannot strip-and-resign; new client detects schema regression and quarantines                                             |

### Redundant Recovery Paths

Restoring a complete asset collection does not depend on any single mechanism. The following paths are **independent** — each is annotated with the failures it survives:

1. **Master-key escrow.** A recovery passphrase or BIP39-style seed unwraps the server-side escrow blob → account master key → AMK escrow → every asset. *Survives: total device loss.* See [Master-Key Escrow](/design/backup-recovery/#master-key-escrow).
2. **Cross-device recovery.** Any signed-in device re-bootstraps a new device over a verified channel. *Survives: partial device loss, and loss of the master-key backup — as long as one device survives.*
3. **Owner Group Key (OGK).** Any current member of the [owner set](#owner-group-keys-ogks) recovers every album's AMK versions, independent of album membership. *Survives: lost album membership, gaps in AMK distribution over MLS.*
4. **Portable backup artifact.** A self-describing, versioned, encrypted archive, stored offline. *Survives: server data loss, account compromise, escrow-blob corruption.* See [Backup Artifact](/design/backup-recovery/#backup-artifact) for the container format.
5. **Recovery-first filesystem rebuild.** CBOR sidecars are the canonical metadata store; the database is a rebuildable query cache. The idempotent `rebuild_index()` (`capsule-core/src/library/rebuild.rs`) walks `.cbor` sidecars and reconstructs the index. *Survives: DB corruption and catastrophic bugs in the index/query layer.*
6. **Content-addressed durability redundancy.** Ciphertext is addressed by the SHA-256 of its bytes, so any byte-identical copy — on another device or a [federated](/design/federation/) peer — is independently verifiable. This is a *durability* path: it restores ciphertext, not keys. *Survives: single-server data loss.*
7. **Trash soft-delete window.** Deletes are soft first — `soft_delete()` / `purge_expired_trash()` (`capsule-core/src/library/trash.rs`) give a reversal window before a hard purge. *Survives: erroneous deletes by a bug or user.*

**Account-type coverage.** Registered accounts have all seven paths. [Delegated/sponsored accounts](/design/authentication/#account-types) are recovered via the sponsoring account's master key, since their keys derive from it. Non-registered (share-link) accounts hold no collection of their own — recovery is not applicable.

### Bug-Resistance Invariants

These cross-cutting properties make recovery robust specifically against *catastrophic bugs*, not just key loss:

- **The backup path is independent of the MLS ratchet.** Restore never reconstructs ratchet state, so a ratchet bug cannot strand data. The master key — not any ratchet state — is the single backed-up root.
- **Hardware-bound, disposable device keys.** Device keys live inside hardware, are non-exportable, and are never backed up — a lost device is re-bootstrapped, not recovered.
- **Cross-signing (Matrix-style).** The master identity signs every device key; adding a device means an existing device signs it, so losing one device never compromises the account.
- **Every construction is versioned.** KDF `info` strings, in-blob Argon2id parameters, the [`crypto_suite_id`](#versioning-identifiers) on every manifest and metadata blob, and the [`sidecar_schema`](/design/metadata/#sidecar-schema-v1) on every sidecar mean a buggy v2 never strands v1 data — v2 keys and structures coexist with v1 without a flag day. Signature coverage of `crypto_suite_id` defeats downgrade-attempts.
- **`verify_asset` quarantines, never drops.** A bug-produced invalid asset is neither silently dropped nor silently accepted; it is quarantined and surfaced in the audit trail so an operator can tell a bug from an attack.
- **Provenance is append-only.** Each `ProvenanceRecord` carries the hash of its predecessor (`prior_provenance_hash`), and every record is hybrid-signed by the producing device. An attacker holding every *current* key still cannot rewrite a past record without forging an earlier (possibly retired) device's signature — history is read-only. See [Provenance of Library Modifications](#provenance-of-library-modifications).
- **Stale-revival is rejected.** An incoming manifest whose `prior_provenance_hash` is behind the receiver's stored `latest_provenance_hash` is treated as stale and quarantined — a deleted asset cannot be silently resurrected by a peer or a backup restore. The check is enforced both client-side and server-side (no key needed); see [Threat Model](/design/threat-model/).
- **Backup verification runs before reliance.** Preview, dry-run, signature-chain, and AMK-completeness checks (see [Backup Verification](/design/backup-recovery/#backup-verification)) detect an incomplete or broken backup *before* it is needed.

## Transport Security

All client-server communication is over HTTPS. While our stack aims to stay PQ-safe (within due course), the transport layer (TLS) must be configured by the server administrator to be PQ-resistant as well. As of writing, the standard is TLS 1.3 with hybrid X25519+ML-KEM key exchange enabled. Since application servers do not terminate TLS, ensure your ingress/reverse proxy is properly configured.

## Implementation

- **Centralized audit paths:** All key cryptographic primitives are centralized in `capsule-core/crypto`. Asset acknowledgement goes through the single `verify_asset` chokepoint (see [Write Authorization](#write-authorization)).
- **Contract-driven development:** Define the crypto interfaces, data structures, and the full set of test cases — especially negative cases — before implementing logic.
- **Backward compatibility:** The server stores all data and metadata encrypted; its database model is distinct from the client's and records `crypto_suite_id` and `protocol_version` for every manifest. Old suite ids and protocol versions remain decryptable forever — retiring a primitive adds an inventory row and a new suite id, never edits or removes an old one. Clients outside the server's supported `protocol_version` range are rejected at the [protocol handshake](/design/threat-model/), before any state is written.
- **Trust the server (and only the server) for storage, never for authorization:** The server owns, provisions, and maintains the encrypted user data, so we rely on it to *hold* data — but authorization decisions are verified cryptographically against MLS-distributed keys, never taken on the server's word.
- **Memory hygiene:** All keys and decrypted data are zeroed in memory immediately after use. We also use secure memory allocation where possible to prevent swapping to disk.

Further guidance:

- Use audited libraries only — libcrux (formally verified), RustCrypto, ed25519-dalek, x25519-dalek; never be the first serious user.
- Use MLS rather than inventing group crypto; it handles the 1:1 case and shifts the audit burden to the IETF and OpenMLS.
- Keep the backup path independent of the ratchet — album keys live in the backed-up hierarchy, so recovery never reconstructs ratchet state.
- Version every key derivation with an `info` string (`"albums/v1"`, `"asset-file/v1"`) so v2 keys can derive alongside v1 without a flag day.
- Store device private keys in hardware (Secure Enclave, StrongBox, TPM) to eliminate memory-extraction attacks.
- Write test vectors against known implementations (libsignal, OpenMLS, RFC vectors) before writing anything novel.

### Versioning

The construction of every encryption metadata structure is always versioned. Parameters (e.g. for Argon2id) must be saved inside the construction to ensure future changes do not break previous constructions.
