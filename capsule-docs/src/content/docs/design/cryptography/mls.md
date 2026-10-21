---
title: MLS Group Membership
description: How Capsule binds MLS (RFC 9420) to its identity layer and uses it for album membership
status: draft
---

Album membership is MLS (RFC 9420) over OpenMLS with the X-Wing [ciphersuite](/design/cryptography/primitives/#mls-ciphersuite) `0x004D`. `OpenMlsAuthority` (`capsule-core/src/crypto/authority/openmls_authority/`) implements [`AlbumAuthority`](/design/cryptography/keys/#write-authority-interface); `ReferenceAuthority` is the deterministic fixture.

- **Private codepoint is fine:** interop is a non-goal. A ratified suite is a re-pin via the [upgrade ceremony](/design/versioning/#album-upgrade-ceremony) and [`crypto_suite_id`](/design/cryptography/primitives/#versioning-identifiers).
- **ChaCha20-Poly1305** ([MLS control AEAD](/design/cryptography/primitives/#mls-control-aead)) protects only control traffic; user data uses [AES-GCM](/design/cryptography/primitives/#bulk-aead).
- **PQ authentication lives at the identity layer.** LeafNodes are Ed25519-only, so identity certificates sign the MLS key with **both** [hybrid](/design/cryptography/primitives/#signature-scheme) halves; peers MUST verify both before admitting a leaf.
- **Blocking a user** removes all their devices in one `Commit`: one epoch bump.

## Membership Operations

Every ceremony is idempotent ([Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants)).

### Add user Bob to album

1. Fetch Bob's [device directory](/design/cryptography/keys/#device-directory) KeyPackages.
2. `Add` + `Commit` with all of his devices as leaves.
3. The `Welcome` carries the current AMK plus older AMKs as the album's [history policy](#history-delivery-for-new-joiners) allows.

### Remove user Charlie

1. `Remove` + `Commit` for all of Charlie's devices. The new MLS epoch excludes him.
2. The committer mints a fresh AMK and distributes it to the remaining members. All later uploads use it.
3. Charlie keeps AMKs he held; past access cannot be undone.

### Add new device for existing member

One of Alice's existing devices adds the new device as a leaf. The `Welcome` carries every AMK Alice is entitled to. First-device enrollment is covered in [Device Enrollment](/design/device-enrollment/).

### Remove lost device

Any of the user's remaining devices issues `Remove`. As with a user removal, the AMK MUST rotate, because the lost device's keys are presumed compromised.

## History Delivery for New Joiners

The wrapper's only custom protocol: `AlbumKeyDistribution { amk_version, amk_bytes }`, broadcast on an epoch bump and batched in a `Welcome`. AMK owner: [Keys](/design/cryptography/keys/#album-master-keys-amks); consumer: [Encryption](/design/cryptography/encryption/#asset-key-derivation).

- **`history_policy` is fixed per album, not per add.** It is set at creation: `Full` (the default for shared albums: every AMK since v1) or `Capped(n)` (only recent epochs; older assets show placeholders). So a user's readable range never depends on who added them. Changing the policy requires an [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony).
- **Epoch ceiling on join.** The joiner takes the highest `amk_version` attested by the Welcome's commit chain as its monotonic ceiling, and rejects any manifest that claims a higher epoch. This keeps it from trusting the server's counter ([Write Authorization](/design/cryptography/keys/#write-authorization)).

## Forward Secrecy & Post-Compromise Security

At-rest ciphertext persists, so true forward secrecy does not apply to it. What each epoch bump guarantees: compromising the current epoch reveals nothing about past epochs, and removed members cannot read future epochs. Forward secrecy for data in transit comes from TLS 1.3 ([Transport Security](/design/cryptography/failure-modes/#transport-security)). Lost commits, divergence and re-keying are covered in [MLS Resilience](/design/mls-resilience/).

## Validation

- **Round trip:** add user, add device, remove user, remove device and AMK rotation, with all members' views agreeing after each commit.
- **Welcome:** `Full` delivers every prior AMK and decrypts every prior asset; `Capped(n)` delivers exactly the last *n*.
- **Policy consistency:** the same user added through different devices or in different orders receives an identical AMK range.
- **Epoch ceiling:** a manifest claiming an epoch above the chain's attested ceiling is rejected.
- **Idempotency:** a replayed commit is rejected and leaves state unchanged.
- **Identity binding:** a LeafNode key without an ML-DSA identity signature is rejected.
- **Concurrent commits (smoke):** parallel proposers converge without splitting the group.

E2E: [Module Map](/design/module-map/#e2e-test-surface).
