---
title: Device Enrollment
description: First-device bootstrap and cross-device add ceremonies for Capsule accounts
---

A Capsule account has one or more devices, each holding a hardware-bound DSK + DEK cross-signed into the user's [device directory](/design/cryptography/keys/#device-directory). This doc owns the two enrollment ceremonies a device can go through to *get into* that directory:

- **[First-device enrollment](#first-device-enrollment).** Brand-new account, no other device exists. The first device generates the master key and the initial device keys.
- **[Cross-device add](#cross-device-add).** An existing signed-in device adds a new device to the directory (the new device gets the master key handed to it over a verified channel).

These are distinct from **[cross-device recovery](/design/backup-recovery/#default-mechanisms)** (which is also a way to bring up a new device, but in the recovery context — the user has lost their other devices and is using the recovery passphrase + master-key escrow to restore).

Implementation will live in `capsule-core::crypto::keys` (key generation and wrapping) and `capsule-api-auth::devices` (the device directory and the enrollment authentication surface). The ceremony glue lives in per-platform native client code (QR scan, biometric prompt).

## First-Device Enrollment

When a user creates a brand-new Capsule account, the very first device runs the full setup ceremony:

1. **Generate the master key.** A 32-byte CSPRNG draw becomes the account master key. It is wrapped under a recovery passphrase via [Argon2id](/design/cryptography/primitives/#password-based-kdf); the wrapped blob is uploaded to the server-side [master-key escrow](/design/backup-recovery/#master-key-escrow). The plaintext recovery passphrase is shown to the user and never persisted.
2. **Generate the User IK.** A hybrid Ed25519 + ML-DSA-65 keypair (the [User Identity Keys](/design/cryptography/keys/#user-identity-keys-user-iks)). The private halves are wrapped under the master key; the public halves go into the (initial, single-member) device directory.
3. **Generate this device's keys.** A DSK (hybrid Ed25519 + ML-DSA-65) and a DEK (hybrid X25519 + ML-KEM-768), both generated inside the hardware secure element and non-exportable. Both are signed by the IK and added to the device directory.
4. **Publish the device directory.** The IK-signed directory is uploaded to the server.
5. **Create the default album.** Establish the owner's [default album](/design/organization/#the-default-album) — a new MLS group at the `album_id` derived from the master key (see [Keys — Key Chain](/design/cryptography/keys/#key-chain)), with this device as the sole admin/writer — and set the owner's `default_album_id` pointer ([Filesystem — Server](/design/filesystem/server/#ownership-partitioning-and-quota)) to it. This guarantees a writable import destination from the first moment the account exists.
6. **Show the recovery passphrase.** This is the only path back into the account if every device is lost, so saving it is **gated, not advisory**: the user must type back a short slice of the passphrase before setup completes, forcing them to actually record it rather than dismiss the screen. The plaintext passphrase is never persisted.

Two design points:

- **Account-creation auth.** The very first request authenticates via the [authentication](/design/authentication/) flow for new registration (OIDC, or the server's own credential ceremony). This establishes the *account* and its server-side metadata only — it confers no data access. All data authority is cryptographic: the master key generated here, and device keys validated device-to-device against the [device directory](/design/cryptography/keys/#device-directory). The server authenticates *who owns the account*; cryptography authenticates *what can read the data*.
- **Multi-device-from-start.** Enrolling a second device right after signup uses the ordinary [cross-device add](#cross-device-add) ceremony — there is no separate "freshly-created" path. One device is signed in and healthy, which is exactly cross-device add's precondition.

## Cross-Device Add

When an existing signed-in device adds a new device to the same account:

1. **Initiate from the existing device.** The user opens "Add another device" on device A (already signed in). Initiating an add requires a **fresh local device authorization** on A (biometric or device passcode) — a valid session token alone is **not** sufficient, so an attacker holding only a stolen session token cannot enroll a rogue device without physical control of A. Device A then generates a one-time **enrollment code** — **single-use, ≥64 bits of entropy, valid 10 minutes**, scoped to this one ceremony, collision-checked at generation, and deleted by the server on redemption or expiry — and displays it as a QR code (with a text fallback).
2. **Scan or enter on the new device.** Device B scans the QR (or types the code).
3. **Establish a short-lived channel.** Devices A and B perform an ephemeral X25519 ECDH to derive a one-time channel key, carried over a **server relay by default, or a direct LAN connection when both devices are on the same network** (discovered via mDNS; LAN preferred — fewer moving parts, no relay trust). The channel is mutually authenticated by the enrollment code plus the ephemeral DH.
4. **Verify the channel.** A short safety code derived from the channel transcript is displayed on both devices, **alongside each device's identity (model + a short key fingerprint)**; the user confirms both that the codes match and that the device being added is the one physically in front of them. Binding the code to device identity defends against a MITM on the relay channel and against a relay that swaps in a different device.
5. **Transfer the master key.** Device A wraps the account master key under the channel-derived key and sends to device B. Device B unwraps, generates its own DSK + DEK in hardware, and presents them to device A for signing.
6. **Cross-sign and publish.** Device A signs B's device keys with the user's IK, updates the device directory, and uploads it. The IK private halves are wrapped under the [master key](/design/cryptography/keys/#registered-accounts), which device A already holds while signed in — so **any fully-enrolled device can unwrap the IK and authorize an add**. There is no special "IK-holder" device or extra key class; holding the master key is the single requirement for identity signing.
7. **B joins MLS groups.** With its keys now in the directory, device B can be added as a leaf to each album's MLS group (via the standard [Add new device](/design/cryptography/mls/#add-new-device-for-existing-member) flow).

Two presentation choices:

- **Enrollment code.** Presented as a QR code with a **friendly numeric** text fallback — the channel is independently authenticated by the safety code, so the code itself only needs to be conveniently transcribable, not dense. Entropy (≥64-bit), single-use, and 10-minute expiry are fixed in step 1.
- **Safety-code check.** Step 4 binds the code to each device's identity (model + key fingerprint). To make the human comparison failure-resistant, both devices show the code in the same chunked, fixed-length format, and confirming requires an explicit match-and-identity acknowledgement on **both** devices — a mismatch is the abort path, not a missed default.

## Relationship to Cross-Device Recovery

Cross-device recovery (owned by [Backup and Recovery](/design/backup-recovery/#default-mechanisms)) is operationally similar — both involve handing the master key to a new device over a verified channel — but the trigger is different:

- **Cross-device add** is *additive*: an existing device is healthy and is bringing up a new sibling.
- **Cross-device recovery** is *substitutive*: every device has been lost; one is being bootstrapped from the recovery passphrase, possibly assisted by a surviving device.

The two ceremonies may share underlying code (channel-establishment, key-transfer wrapping) but the entry surfaces and the user expectations are distinct.

## Contract Skeleton

```rust
// in capsule-core::crypto::keys
fn first_device_setup(passphrase: &str) -> Result<EnrollmentResult, EnrollmentError>;

// in capsule-api-auth::devices
fn issue_enrollment_code() -> EnrollmentCode;       // server stores a short-lived record
fn redeem_enrollment_code(code: EnrollmentCode) -> Result<ChannelHandle, EnrollmentError>;

// on the existing device
fn complete_cross_device_add(channel: ChannelHandle, b_keys: DeviceKeyBundle) -> Result<(), EnrollmentError>;
```

The channel and enrollment-code wire formats are an implementation detail; channel dispatch is LAN-direct when both devices share a network and server-relay otherwise (step 3).

## Failure Modes

Each enrollment ceremony must handle:

- **User abandons mid-flow.** The enrollment code expires; no state is persisted; the user starts over.
- **Channel hijack attempt.** The safety-code verification catches an active MITM on the channel; if codes don't match, the user is told to abort.
- **Stolen session token.** A session token alone cannot start an add — the fresh local device authorization (step 1) gates initiation on physical control of an already-trusted device, so a remotely-exfiltrated token cannot enroll a rogue device.
- **Hardware-key generation failure.** The new device's secure element refuses to generate keys (rare but happens); enrollment fails with an actionable error.
- **Server unavailable.** The directory upload fails; the new device is locally functional but invisible to other devices until the upload succeeds. The client retries with backoff and surfaces "finishing setup — will complete when the server is reachable"; the device's keys are already generated and valid, so the delay loses nothing.
- **Default-album creation fails.** Account creation still completes — the master key, identity, and device are fully valid. Because the [default album](/design/organization/#the-default-album)'s ID is [derivable from the master key](/design/cryptography/keys/#key-chain), any device recreates it lazily before the first import, so a transient failure here never blocks setup or loses data.

## Validation

- **First-device setup round-trip (smoke).** Run the full ceremony; assert master key wrapped + escrowed; assert device directory has exactly one entry; assert recovery passphrase unwraps the escrow.
- **Cross-device add safety-code check (unit).** Inject mismatched safety codes; assert the ceremony aborts.
- **MITM defense (smoke).** Mock a relay that swaps the channel keys; assert safety codes diverge; assert abort.
- **Enrollment-code expiry (unit).** Generate code; let it expire; assert redemption fails with the right structural error.
- **Enrollment-code single-use (unit).** Redeem a code; attempt to redeem it again; assert rejection; assert the server deletes it on both redemption and expiry.
- **Local-auth gate (unit).** Attempt to initiate a cross-device add with only a session token and no fresh local device authorization; assert refusal.
- **Hardware-key failure (smoke per platform).** Mock hardware-element refusal; assert the ceremony surfaces a clear error rather than partially completing.
