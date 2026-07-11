---
title: MLS Group Membership
description: How Capsule binds MLS (RFC 9420) to its identity layer and uses it for album membership
status: draft
---

Capsule's group layer is the [MLS ciphersuite](/design/cryptography/primitives/#mls-ciphersuite) from the inventory. It will be implemented in `capsule-core::crypto::mls` (planned) as a thin wrapper over OpenMLS — the wrapper is what binds MLS to Capsule's identity layer ([Keys](/design/cryptography/keys/)) and to the in-band AMK distribution.

**Status: buildable — the suite ships today.** Capsule adopts `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (codepoint `0x004D`), the X-Wing suite [OpenMLS](https://github.com/openmls/openmls) ships today via its formally-verified `libcrux` provider. Live MLS is gated only by implementation effort now, not by anything upstream. The reasoning behind the pin (decided 2026-07):

- **A missing IANA codepoint is not a blocker here.** Capsule is a closed deployment — every client is Capsule's own, and federation is Capsule-server↔Capsule-server — so cross-implementation interop, the thing an IANA codepoint buys, is a non-goal. OpenMLS's private/experimental `0x004D` is sufficient.
- **X-Wing being off the IETF standards track is a migration question, not a gate.** The WG-adopted `draft-ietf-mls-pq-ciphersuites` moved to *direct* ML-KEM hybrid suites (AES-GCM, SHA-256/384); X-Wing-in-MLS is the separate, non-adopted `draft-mahy-mls-xwing`. If Capsule ever needs the ratified suite, it re-pins via the [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony) + [`crypto_suite_id`](/design/cryptography/primitives/#versioning-identifiers) agility — a one-line inventory edit behind the same seam, not a redesign.
- **Staying on OpenMLS.** It is independently audited (SRLabs, 2026), maintained by Phoenix R&D + Cryspen, in production (Wire, XMTP, Cloudflare Meet), and the only MLS library shipping any PQ suite; the alternative (`mls-rs`) has no third-party audit and no PQ support.

Until the `OpenMlsAuthority` wrapper lands, the epoch-authority role this doc assigns to the MLS commit chain is filled offline by the admin-signed epoch ledger behind the [`AlbumAuthority` interface](/design/cryptography/keys/#write-authority-interface), which drops in without touching `verify_asset`; the membership ceremonies, `Welcome`/history delivery, and the [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony) are scheduled implementation work rather than an upstream wait. Docs that depend on live MLS link to this note rather than restating it.

The ciphersuite's choice of [ChaCha20-Poly1305](/design/cryptography/primitives/#mls-control-aead) (rather than the [AES-GCM](/design/cryptography/primitives/#bulk-aead) used for user data) is acceptable because:

- It only protects MLS's own control messages (kilobytes of membership and key data, not your photos).
- ChaCha20-Poly1305 is one of the two most-audited AEADs in existence.
- The alternative is a classical-only MLS ciphersuite plus a hand-rolled PQ retrofit — exactly the custom crypto we are trying to avoid.

One follow-on: MLS binds LeafNode signatures to Ed25519 in this suite, so the ML-DSA half of the [hybrid signature scheme](/design/cryptography/primitives/#signature-scheme) lives at the **identity layer** — identity certificates sign the Ed25519 MLS key with both Ed25519 and ML-DSA, and peers verify both before accepting a device into a group. This keeps MLS pure while preserving PQ authentication end-to-end.

For the broader principle of preferring MLS over custom group crypto: it handles the 1:1 case, shifts the audit burden to the IETF and OpenMLS, and gives forward secrecy + post-compromise security ([below](#forward-secrecy--post-compromise-security)) as a property of the ratchet rather than something Capsule has to reinvent.

## Membership Operations

The four lifecycle ceremonies the wrapper exposes. Each is an idempotent entry-point: replaying the same proposal produces the same group state (MLS commits are ordered by the chain, and OpenMLS rejects duplicates at the protocol layer — see [Threat Model — Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants)).

### Add user Bob to album

1. Fetch Bob's [device directory](/design/cryptography/keys/#device-directory) (list of his devices with KeyPackages published to the server).
2. MLS `Add` proposal + `Commit` adding all Bob's devices as leaves.
3. The `Welcome` message to Bob's devices carries current `AMK_v_current` as a Welcome extension.
4. If full history is desired (usually yes for shared albums), also include `AMK_v1..AMK_{current-1}` in the Welcome — Bob's devices can now decrypt everything.
5. If only post-join history, omit older AMKs — Bob sees only future photos.

### Remove user Charlie

1. MLS `Remove` proposal + `Commit` removing all Charlie's devices.
2. MLS advances to a new epoch; Charlie's devices can no longer read MLS traffic.
3. Committer generates fresh `AMK_v{current+1}` and broadcasts via MLS to remaining members.
4. All future photo uploads use `AMK_v{current+1}`.
5. Charlie retains `AMK_v1..current` locally, so he can still decrypt photos he *already had access to* — correct behavior (he already had those photos; nothing you do after removal un-seeds them). But new uploads are invisible to him.

### Add new device for existing member

1. Alice's existing device adds Alice's new device as a leaf in the MLS group.
2. Welcome carries all AMK versions Alice is entitled to.
3. New device is now equivalent to Alice's other devices.

For first-device enrollment (a brand new account with no other device), see [Device Enrollment](/design/device-enrollment/).

### Remove lost device

1. Any of the user's remaining devices issues MLS `Remove` for the lost device.
2. Treat like a removal above — bump AMK version, since you must assume the lost device's keys are compromised.

## History Delivery for New Joiners

The one spot where the wrapper writes real custom code. AMK delivery — both the steady-state broadcast on an epoch bump and the batch form inside a `Welcome` — rides one application message whose shape this doc owns: `AlbumKeyDistribution { amk_version, amk_bytes }` ([Encryption](/design/cryptography/encryption/#asset-key-derivation) consumes it; the AMK itself is owned by [Keys](/design/cryptography/keys/#album-master-keys-amks)). Two delivery patterns:

**Full history (recommended for shared albums):** Welcome message carries an encrypted blob of `[AMK_v1, AMK_v2, ..., AMK_current]`. The new joiner decrypts all, can now read every photo.

**Capped history (e.g., last 90 days):** Only include AMKs corresponding to epochs ≥ threshold. Older photos remain visible but not decryptable — show placeholders.

Matrix supports both; most photo-sharing products default to full history. **Capsule fixes the policy per album**, not per add: `history_policy` is part of the album's MLS metadata, set at album creation (full history is the default for shared albums; capped history is the opt-in). Every `Add` into that album applies the album's declared policy, so a member's history visibility never depends on which device added them or in what order — eliminating the divergence where the same user ends up able to decrypt different ranges on different devices. Changing an album's `history_policy` is an [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony), never an ad-hoc per-add decision.

**Epoch ceiling on join.** The Welcome's commit chain is also the joiner's authority on the album's *current* epoch: the new member adopts the highest `amk_version` the admin-signed chain attests as its monotonic ceiling and rejects any later manifest claiming a higher epoch. This is what lets a brand-new client enforce `amk_version` monotonicity without trusting the server's counter (see [Write Authorization](/design/cryptography/keys/#write-authorization)).

## Forward Secrecy & Post-Compromise Security

The MLS-based scheme provides forward secrecy (FS) and post-compromise security (PCS). The specific implementation is MLS (RFC 9420) with the X-Wing PQ KEM suite (`0x004D`, `draft-mahy-mls-xwing`).

**Clarification:** True FS on data-at-rest is a contradiction (the ciphertext persists). What MLS gives you at each epoch bump is: a compromise of the current epoch's keys does not help an attacker read past epochs, and removed members cannot read future epochs. That is the practical security property you want.

For data-in-transit between clients and server (uploads, key-bundle fetches), use TLS 1.3 with ephemeral ECDHE — that is where session-level FS lives. See [Transport Security](/design/cryptography/failure-modes/#transport-security).

## Notes on Scaling

MLS scales to thousands of leaves, so even a 50-user album (200+ devices) is fine. Every `Commit` touches the whole tree and each `Welcome` carries `log(N)` path secrets plus the AMK blob — a cost to watch for very large shared albums.

## Resilience to Edge Cases

MLS can encounter a state-divergence or lost-commit scenario that the basic protocol does not solve — handling those (group re-keying, repair after partition, reconciliation of two divergent commit chains) is owned by [MLS Resilience](/design/mls-resilience/).

## Validation

- **Protocol round-trip** — unit tests run the four ceremonies against an in-process OpenMLS group: add user, add device, remove user, remove device, AMK rotation. Asserts every member's view of the group state matches after each commit.
- **Welcome correctness** — unit test that a Welcome for a new joiner with `full_history = true` contains every prior AMK and decrypts every prior asset; with `capped_history = N`, contains only the last N epochs.
- **History-policy consistency (unit).** Add the same user via two different devices/orders against an album with a fixed `history_policy`; assert both Welcomes deliver the identical AMK range — the policy is read from album metadata, not chosen per add.
- **Epoch ceiling from chain (unit).** Construct a Welcome whose commit chain attests epoch N; assert the joiner adopts N as its monotonic `amk_version` ceiling and rejects a subsequently-presented manifest claiming epoch N+1 that the chain does not attest.
- **Idempotency** — replay the same commit twice; OpenMLS rejects the second; group state unchanged.
- **MLS + identity binding** — smoke test that the wrapper rejects a LeafNode whose Ed25519 key is not also covered by an ML-DSA signature at the identity layer (the hybrid binding from [primitives](/design/cryptography/primitives/#signature-scheme)).
- **Concurrent commits** — smoke test that two clients proposing in parallel converge after MLS's commit-ordering resolution; no group splits.

The ceremony-level cross-module test (full enroll + add to album + upload as a real client) is the bounded E2E case listed in [Module Map](/design/module-map/#e2e-test-surface).
