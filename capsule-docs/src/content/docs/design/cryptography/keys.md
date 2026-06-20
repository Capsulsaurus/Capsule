---
title: Key Management
description: Capsule's key hierarchy, device coordination, and write authorization
---

Capsule's keys form a single hierarchy with one backed-up root. The hierarchy is implemented in `capsule-core::crypto::keys`; hardware-bound storage adapters (Secure Enclave, StrongBox/Keystore, TPM) live in per-platform glue under `capsule-sdk::hardware-keys`.

- The **account master key** is the only key that is escrowed/backed up. It does not encrypt assets directly. Its job is to (1) wrap the per-device identity private keys and (2) anchor the encrypted backup that escrows album keys.
- **Device keys** are hardware-bound, non-exportable, and therefore disposable — a device is re-bootstrapped from the master key rather than recovered.
- **Album keys** (AMKs) are random per-epoch keys ledgered in MLS, escrowed both in the master-key backup and in the [Owner Group Keys](#owner-group-keys-ogks).

The guiding rule is **the backup path is independent of the MLS ratchet**, so losing every device but holding the recovery passphrase still restores every photo. We deliberately avoid the Matrix failure mode where undecryptable content is routine. See [Failure Modes](/design/cryptography/failure-modes/).

## Key Chain

The account master key does **not** derive album keys — albums are MLS groups with random AMKs. The master key's role is to wrap device identity keys and to anchor the encrypted backup that escrows AMKs:

```plaintext
account_master_key (backed up — see Failure Modes)
  ├─ wraps device identity private keys (IK / DSK / DEK private halves)
  └─ anchors the encrypted backup that escrows:
        AMK_v{n}  (random 32 bytes, per album, minted per MLS epoch)
          └─ HKDF-SHA512(ikm=AMK_v{n}, salt=file_id||nonce_prefix, info="asset-file/v1") → 32-byte AES file key
                └─ AES-256-GCM-STREAM
```

Construction rules (consistent with the [KDF choice](/design/cryptography/primitives/#key-derivation)):

- Always include a version string in `info` so the KDF can be rotated later.
- Salt with something unique per scope (`album_id`, `file_id`) — never reuse salts across scopes.
- The 512-bit KDF output is truncated to 32 bytes for the AES-256 file key.
- Each **encryption** gets a fresh derived key: the per-encryption `nonce_prefix` is folded into the `salt` (`file_id || nonce_prefix`; see [Encryption — Asset Key Derivation](/design/cryptography/encryption/#asset-key-derivation)), so even a same-`file_id` [`replace`](/design/authorization/#the-closed-action-set) under the same epoch re-rolls the key, never merely the nonce. The STREAM nonce can therefore safely start at zero per encryption, and no `(file_key, nonce_prefix)` pair is ever reused.

The master key also derives one **identifier** — the [default album](/design/organization/#the-default-album)'s `album_id`, via HKDF with a dedicated `info` label — so any device can recompute which album is the de facto default from the master key alone, even after recovery. This derives an *ID*, not a key: the default album is an ordinary album with a random per-epoch AMK like any other.

Per-album AMKs are escrowed in the server-side encrypted backup (see [Backup and Recovery](/design/backup-recovery/)) and the [OGK](#owner-group-keys-ogks) — not derived from MLS ratchet state — so losing all devices but holding the recovery passphrase still restores photos. Ratchet keys are expected to be ephemeral.

## Key Generation

All key generation happens client-side, drawn from the [OS CSPRNG](/design/cryptography/primitives/#randomness). The scheme is PQ-safe ("post-quantum"): classical + PQ primitives combined so that breaking either alone does not break security.

### User Identity Keys (User IKs)

A User IK is generated once per user ever, and lives forever (or until account compromise). This is the root of trust and signs everything below it. It is always verified out-of-band or via safety numbers.

A User IK is a hybrid **Ed25519 + ML-DSA-65** signing keypair generated entirely on the client at account creation. The private halves are wrapped under the [account master key](#registered-accounts) and never leave the client in the clear; the public halves are published in the signed [device directory](#device-directory).

Revocation is a global account reset — irreversible, non-recoverable, nuclear. It is published as a separate revocation certificate, hybrid-signed by the IK itself, to a well-known location so clients can check for it.

### Device Keys

Each device's keys are cross-signed into the [device directory](#device-directory) by the user's IK:

1. **DSK** (Device Signing Key): hybrid **Ed25519 + ML-DSA-65**.
2. **DEK** (Device Encryption Key): hybrid **X25519 + ML-KEM-768**.

Both are signed by the IK (hybrid signature). Device private keys are **generated inside and never leave hardware** — Secure Enclave (iOS), StrongBox/Keystore (Android), TPM (desktop) — and are non-exportable. Because they cannot be backed up, devices are treated as disposable: a lost device is removed and a new one re-bootstrapped from the master key.

A device key can be revoked without affecting the user's identity or other devices. Revocation is done by signing a revocation statement with the IK and publishing it to a well-known location. The server then refuses to deliver new key wraps to that device, and remaining devices rotate any group keys the revoked device had access to. The revoked device's directory entry is **retained** — marked with `revoked_at` (RFC3339), never deleted — so the manifests it signed *before* revocation stay verifiable forever (provenance is append-only; see [Provenance](/design/cryptography/provenance/#what-an-attacker-with-all-current-keys-still-cannot-do)).

### Owner Group Keys (OGKs)

Assets' `owner_id` maps to a set of users; treat each owner as an MLS group.

- **Type:** Symmetric AES-256 root key of an MLS group whose members are the owner's user set.
- **Purpose:** A recovery/escrow layer. The OGK does **not** wrap individual file keys. Instead, it escrows every album's [AMK versions](#album-master-keys-amks), so any current owner member can recover every album key — and therefore every asset — independent of album membership. This avoids double-wrapping each file while still guaranteeing the owner never loses access.
- **Epoch:** Bumps on any owner-set change. Every member's client commits to MLS, producing a new OGK; the server stores the welcome/commit messages.
- **Revocation:** Remove a user from the owner set → MLS Remove proposal → new epoch → the removed user's device can no longer derive future OGKs and is dropped from future AMK escrow.

### Album Master Keys (AMKs)

Each album is its own MLS group. Members = users with any permission on the album.

- **Type:** Random 32-byte symmetric key, minted per epoch. AMKs are *not* derived from MLS epoch state (which is complicated at edge cases) — they are random keys distributed *over* MLS application messages and ledgered.

Capsule separates **secrecy** (enforced by encryption) from **authorization** (enforced by signatures). One content key plus two signing capabilities, to minimize the surface that can leak:

- **`AMK` — the content key.** Read access. MLS delivers it to *all* album members. Holding it lets you decrypt; not holding it means you cannot.
- **Write capability — a per-epoch write-tier signing keypair.** A **hybrid Ed25519 + ML-DSA-65** keypair (see the [signature scheme](/design/cryptography/primitives/#signature-scheme); both halves must verify). Distributed via MLS to writers only, used to sign [asset manifests](/design/cryptography/provenance/#asset-manifest). It rotates with the AMK epoch, so a removed writer cannot sign for future epochs. This is authorization, not secrecy — see [Write Authorization](#write-authorization).
- **Admin capability — an admin-tier signing keypair.** Also **hybrid Ed25519 + ML-DSA-65**. Distributed to admins only; used to sign MLS membership commits.

Epoch bump triggers: member add/remove, permission change, scheduled rotation (e.g., every 30 days for long-lived albums).

## Write Authorization

A device signature on an [asset manifest](/design/cryptography/provenance/#asset-manifest) proves *which device* produced an asset — but not that the device was *authorized to write* to that album at that time. The server is **not trusted for authorization**: it could replay, reorder, or surface an asset signed by a reader-only device, a removed writer, or a device acting outside its write window. A bug could also produce such an asset. Both must be rejected robustly, with the verification logic kept small enough to be hard to get wrong.

This is **the** contract every consumer of `capsule-core::crypto` depends on. It is invoked from import, sync, federation, peering, and backup-restore — anywhere an asset enters the local trusted set.

- **Epoch-bound write proof.** Every asset manifest carries, in addition to the device DSK signature, a signature under the album's **per-epoch write-tier signing key**. Only writers at that epoch hold that key. The manifest's `amk_version` identifies the epoch.
- **Authorization authority is MLS history, not the server.** The client verifies the write-tier signature against the write-tier public key it learned for that epoch *from MLS* — the album's MLS commit chain (admin-signed) is the sole authority on who could write when. A server-asserted authorization is never sufficient.
- **Epoch ceiling is MLS-attested, not server-asserted.** The monotonic `amk_version` ceiling a client enforces is derived from the album's admin-signed [MLS commit chain](/design/cryptography/mls/#membership-operations) — the same authority that admits writers — never from the server's stored counter. A brand-new client learns the current epoch from the MLS group state delivered in its [Welcome](/design/cryptography/mls/#history-delivery-for-new-joiners), then rejects any manifest whose `amk_version` exceeds that MLS-attested epoch, so a server can neither fabricate a future epoch nor rewind to an old one and have a client honor it. The server's own no-key monotonicity check (invariant 18 in [Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants)) is a structural backstop, not the authority — it stops a *client* from skipping epochs, while MLS stops the *server* from fabricating them.
- **What this accepts vs. rejects.** An asset signed by a writer who was *later* removed is still acknowledged — it was valid when written, and nothing after removal un-seeds it. An asset signed at an epoch where the signer lacked write capability is **rejected**: an attacker (or a buggy/colluding server) cannot produce a valid write-tier signature for an epoch they were not a writer in.
- **Backdating buys nothing.** Ordering and authorization ride the provenance hash-chain and the `amk_version` epoch, never the self-asserted `timestamp` (see the timestamp note below). The "pre-sign backdated assets, then upload them after removal" attack therefore fails on the *epoch*, not the clock: a manifest must carry a `write_sig` for the epoch it claims, and a removed writer holds no write-tier key for any epoch past their removal. Anything they upload afterward either names an old epoch the chain has already advanced beyond (rejected by the monotonic `amk_version` + chain-head checks) or a new epoch they cannot sign for.
- **Single verification chokepoint.** All of this lives in one `verify_asset(manifest, ciphertext, mls_state)` function in `capsule-core::crypto`. It is the only path by which a client acknowledges an asset, and per [contract-driven development](/design/principles/) it ships with exhaustive negative test cases: reader-signed, removed-writer, wrong-epoch, forged certificate chain, replayed manifest.
- **Crypto validity, not durability.** `verify_asset` proves an asset is cryptographically valid and authorized; it says nothing about whether the *server* still durably holds the bytes. Confirming durable, indexed, retrievable server storage — the precondition for any destructive local cleanup — is a separate, key-free query, the [storage-verification endpoint](/design/import/storage-verification/). A safe local release requires **both** to pass.
- **Defensive failure handling.** A verification failure is *never* silently dropped and *never* silently accepted. The asset is quarantined and surfaced in the [provenance/audit trail](/design/cryptography/provenance/#provenance-of-library-modifications) so an operator can distinguish a bug from an attack after the fact. This bounds the blast radius of an implementation bug.
- **Transient vs. terminal outcomes.** `verify_asset` returns one of three outcomes, not two: **accept**, **terminal-reject** (reader-signed, removed-writer, wrong-epoch, forged chain, suite-downgrade → quarantined as above), and **pending**. *Pending* is the narrow, recoverable case where the manifest's `amk_version` is within the MLS-attested epoch range but the corresponding AMK content key has not yet arrived over the in-band [AlbumKeyDistribution](/design/cryptography/encryption/#asset-key-derivation) message (an epoch bump whose key broadcast is still in flight). A pending asset is **held and retried** as MLS state catches up — never quarantined as forged and never accepted unverified — until the key arrives or a configurable timeout elapses, after which it escalates to a surfaced quarantine. This distinction stops an in-flight epoch bump from flagging honest concurrent uploads as attacks; see [Failure Modes](/design/cryptography/failure-modes/#failure-mode-catalog).
- **Downgrade-resistant.** Both signatures cover `crypto_suite_id`, `protocol_version`, and `prior_provenance_hash`. A manifest cannot be silently re-signed under a weaker suite or back-dated onto a different chain position without breaking either signature; an attempt is rejected at the same `verify_asset` chokepoint.
- **Timestamp is audit-only.** A manifest's `timestamp` is the client's self-asserted capture/write time and is **never** load-bearing for authorization or ordering — those ride the epoch and the chain above. The server stamps its own trusted `received_at` in the [server-visible envelope](/design/filesystem/server/#postgresql-what-the-server-knows) as the authoritative wall-clock for any time-based policy (retention, rate limits); the client `timestamp` is preserved verbatim for display and audit. A server-side *sanity* bound on `timestamp` is a gross-drift guard for honest clients, **not** an authorization control — it surfaces a wildly-wrong clock rather than silently distorting the audit trail. Grammar owned by [Threat Model — Schema Rules](/design/threat-model/schema-rules/) and mirrored in [Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants).

## Device Directory

Each user publishes a signed device directory. Other users (and federated peers) read it to learn which devices belong to whom and which public keys to trust.

```rust
DeviceDirectory {
  user_id,
  directory_version: u64,        // monotonic; +1 on every change (add, revoke, rotate)
  updated_at:        RFC3339,
  devices: [
    { device_id, ed25519_pk, mldsa_pk, key_package_ref, added_at, revoked_at, signed_by_master },
    ...
  ],
  signature: Hybrid(master_ed25519, master_mldsa)   // covers directory_version + updated_at
}
```

When Alice's device A1 adds Bob to an album, it fetches Bob's directory, verifies the hybrid signature against Bob's published master identity, and adds all Bob's listed devices. Alice's other devices (A2, A3) see the MLS commit and update local state — MLS handles idempotent application of commits, so this just works.

Concurrent edits (A1 and A2 trying to add different people simultaneously) are handled by MLS's proposal/commit ordering — one wins, the other re-proposes on top. OpenMLS exposes this.

**Monotonic version (anti-rollback).** The directory is the trust anchor every peer reads to learn which device keys are current, so a server that could silently serve a *stale* directory — one that still lists a revoked device, or omits a freshly-added one — would undo a revocation or hide a device. `directory_version` closes this: it is master-signed and **strictly monotonic**, and every reader (local client, [federated](/design/federation/) peer, [peering](/design/peering/) handshake) caches the highest version it has seen per user and **refuses a directory whose `directory_version` is below that high-water mark**, surfacing the regression rather than applying it. A reader with no cached version trusts-on-first-use and pins from there. This makes a revocation durable: once a peer has seen the post-revocation directory, the server cannot walk it back. The check is the directory-layer counterpart of the [stale-revival defense](/design/import/download-sync/#stale-revival-detection) for manifests, and is enforced as a [client-](/design/threat-model/validation/#client-side-validation-invariants) and [server-side](/design/threat-model/validation/#server-side-validation-invariants) invariant.

The directory entry's `added_at` field is what blocks the damage scenario where a new device claims its key is older than the account itself: a server rejects an upload from a device whose `added_at` postdates the manifest's `timestamp`. See [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants).

## Identity-Based Key Derivation

Since all assets are encrypted via keys ultimately recoverable from an account's master key, identity keys are encapsulated differently depending on the [account type](/design/authentication/#account-types).

### Registered accounts

Most users have their own unique master key. It is **generated client-side** at account creation from the OS CSPRNG. The server never holds the naked master key. Each device stores its own copy wrapped under that device's DEK; a new device obtains the master key either via [cross-device recovery](/design/backup-recovery/#recovery-mechanisms) or by unwrapping the [encrypted server-side backup](/design/backup-recovery/#master-key-escrow) with the recovery passphrase.

### Delegated/Sponsored accounts

A sponsored account is anchored under the sponsor's master key but holds its own encryption keys. The mechanism — and the only sound way to revoke — is:

1. **Per-sponsoree KEK.** When a sponsor creates a sponsored account, the sponsor draws a fresh 32-byte **sponsoree KEK** from the CSPRNG (it is *not* derived from the master key — a deterministic derivation would be reproducible by the sponsor at any future point, defeating revocation). The KEK is wrapped under the sponsor's master key and stored in the sponsor's escrowed hierarchy.
2. **Sponsoree key material.** The sponsoree's own identity, device, and album keys are generated normally. Their private halves are wrapped under the sponsoree KEK rather than directly under the sponsor's master key, so the sponsor can re-wrap or destroy a single sponsoree's keys without touching its own or the other sponsorees'.
3. **Shared-asset access.** Sponsorees gain access to a sponsor's shared albums via ordinary MLS membership (the sponsoree's devices are added as MLS leaves in the sponsor's album groups). The KEK is *not* a content key — it only wraps the sponsoree's private keys.
4. **Revocation.** Revocation is a three-step operation, all signed by the sponsor's IK:
   - **Rotate** the sponsoree KEK: draw a new KEK, re-wrap surviving sponsorees if any, drop the old KEK.
   - **Publish** an IK-signed revocation certificate naming the revoked sponsoree's identity and the timestamp.
   - **Remove** the revoked sponsoree's devices from every MLS group they were a member of (album groups, owner group) via the standard MLS Remove flow, bumping AMK epochs.

The sponsor's *own* master key is untouched by any sponsoree revocation. The published revocation certificate is what clients and [federated](/design/federation/) peers check to refuse traffic from a revoked sponsoree.

#### Damage bound under sponsor compromise

A compromised sponsor holds every sponsoree's KEK, which wraps that sponsoree's private identity and device keys. It can therefore impersonate a sponsoree's device — forge its `device_sig`, append to or rewrite the sponsoree's [provenance history](/design/cryptography/provenance/#provenance-of-library-modifications), and write under the sponsoree's albums. Unlike a registered account — whose past records are protected even against a full current-key compromise because retired device keys are hardware-bound and non-recoverable — a sponsoree's history is **not** independent of its sponsor. This is inherent to delegation and is **bounded, not eliminated**:

- **The trust is explicit and directional.** A sponsoree's integrity is, by construction, only ever as strong as its sponsor; its keys derive from the sponsor's hierarchy and it is never promised independence. A sponsoree is the right model only when that trust already holds (a family member, a managed device).
- **The blast radius stops at the sponsor's own sponsorees.** Per-sponsoree KEKs (step 1) mean a compromise cannot cross to a *different* sponsor's sponsorees, and registered users an album is shared with verify the sponsoree's published [device directory](#device-directory) and provenance like any peer — they are unaffected.
- **Revocation is clean.** The IK-signed revocation certificate (step 4) cuts off a sponsoree, and federated peers refuse a revoked sponsoree's traffic.
- **Escape hatch.** A user who needs provenance integrity that survives sponsor compromise must hold a **registered account** with hardware-bound device keys that are not derivable from any sponsor KEK. Sponsored accounts deliberately trade that independence for managed simplicity; the choice is the user's to make at account creation.

### Non-registered accounts

**Reading.** Since key management operates at the user level, userless [share links](/design/share-links/) are handled distinctly. We encapsulate the decryption keys around the secret stored in the link. The owner can optionally attach a password, in which case the [password-based KDF](/design/cryptography/primitives/#password-based-kdf) adds a second encapsulation layer on top of the link secret.

**Writing.** Writing is **not supported** for non-registered accounts. Every uploaded asset must be encrypted under an album key and signed with a write-tier key; a non-registered user has neither a device encryption key (DEK) nor a place in any album's MLS group, so it cannot produce a valid [asset manifest](/design/cryptography/provenance/#asset-manifest). Supporting guest uploads would require an ephemeral link-scoped key hierarchy; this is a deliberate non-goal to keep the design simple.

## Key Rotation and Revocation

- **Master key rotation.** The master key can be replaced at will. Rotation re-wraps the key hierarchy (device-key wraps and the AMK escrow blob) under the new master key; the old master key is retained only long enough to complete the re-wrap, then discarded. Existing signed-in sessions hold device and derived keys directly and are **unaffected** — they keep working through the rotation.
- **Device revocation.** Handled via the [device key](#device-keys) revocation certificate plus an MLS `Remove` for that device's leaves (see [MLS — Membership Operations](/design/cryptography/mls/#membership-operations)).
- **Album-member revocation.** Handled by an MLS `Remove` and an AMK epoch bump (see [MLS — Membership Operations](/design/cryptography/mls/#membership-operations)).

### Master-Key Compromise

The hierarchy deliberately wraps *downward* — the master key wraps device identity keys and anchors the AMK escrow; it never wraps individual file keys. AMKs are not derived from it, and compartmentalization is per album (one AMK lineage per album). This is what makes recovery sound: it is **not** inverted to wrap AMKs under the hardware-bound device keys, because device keys are non-exportable and disposable — wrapping AMKs under them would forfeit the [recovery-first guarantee](/design/cryptography/failure-modes/) that holding the recovery passphrase restores every photo after every device is lost.

A suspected master-key compromise is therefore recovered in two moves, not a hierarchy redesign:

1. **Rotate the master key** (above), re-wrapping the hierarchy so the attacker's copy no longer unwraps current device or escrow material.
2. **Re-key the affected albums** — an MLS epoch bump per album mints fresh AMKs and write-tier keys (see [MLS Resilience — Group re-keying](/design/mls-resilience/#group-re-keying-ceremony)), so every future write uses keys the attacker never held.

Ciphertext the attacker already exfiltrated under the old AMKs stays readable to them — inherent to any E2EE system once keys leak — but the blast radius is bounded to the albums whose AMKs were exposed, and all forward writes are clean.

## Validation

- **Derivation determinism** — unit tests assert that HKDF over the same `(AMK, salt, info)` produces byte-identical output, across platforms (no endianness drift, no truncation differences). Cross-checked against RFC 5869 test vectors.
- **Hardware-bound storage round-trip** — per-platform smoke harness: generate a DSK inside the Secure Enclave / StrongBox / TPM, sign a fixed payload, verify the signature against the published public key. Non-exportability is asserted by attempting to read the private bytes and confirming failure.
- **Rotation ceremony** — smoke test the full master-key rotation against a real wrapped escrow blob: rotate, re-unwrap with the new passphrase, confirm every device wrap and AMK escrow entry is recoverable.
- **`verify_asset` negative cases** — exhaustive unit-test surface owned by [Provenance](/design/cryptography/provenance/). Covers reader-signed, removed-writer, wrong-epoch, forged certificate chain, replayed manifest, suite-downgrade. Reuses the AMK + write-tier key fixtures defined here.
- **`verify_asset` pending outcome (unit).** Present a manifest whose `amk_version` is within the MLS-attested epoch range but whose AMK key is not yet locally held; assert the outcome is *pending* (held + retried), not a quarantine. Deliver the key; re-run; assert acceptance. Push the `amk_version` past the MLS-attested epoch; assert terminal-reject.
- **Directory monotonicity (unit).** Cache a `DeviceDirectory` at `directory_version = N`; present one at `N-1` (e.g. still listing a revoked device); assert it is refused and surfaced, not applied. Present `N+1`; assert acceptance and high-water-mark advance.
- **Write-tier signature is hybrid (unit).** Verify a `write_sig` with only the Ed25519 half valid and the ML-DSA half corrupted; assert rejection (both halves required), mirroring the `device_sig` hybrid check.
