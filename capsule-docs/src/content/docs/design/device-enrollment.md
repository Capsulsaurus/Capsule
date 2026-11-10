---
title: Device Enrollment
description: First-device bootstrap and cross-device add ceremonies for Capsule accounts
status: draft
---

Each device holds a DSK + DEK cross-signed into the user's device directory ([](/design/cryptography/keys/#device-directory)). Two ceremonies put a device there. Recovery after losing every device is cross-device recovery ([](/design/backup-recovery/#default-mechanisms)), not enrollment.

**Trust split:** account-creation auth ([](/design/authentication/)) establishes *who owns the account* and confers no data access. Data authority is cryptographic: the master key and device keys verified against the directory.

## First-Device Enrollment

1. **Master key** from the OS CSPRNG ([](/design/cryptography/primitives/#randomness)), shaped per [](/design/cryptography/keys/#registered-accounts), wrapped under the recovery secret and uploaded as the master-key escrow ([](/design/backup-recovery/#master-key-escrow)).
2. **User IK** (composition: [](/design/cryptography/keys/#user-identity-keys-user-iks)), with private halves wrapped under the master key and public halves in the directory.
3. **Device keys** (composition: [](/design/cryptography/keys/#device-keys)), signed by the IK. The classical half MUST be non-exportable in the secure element where one exists. Secure elements offer only P-256, so the hardware path is the planned P-256 hybrid with the PQ half software-sealed under hardware-protected material. Until then both halves are software.
4. **Publish** the IK-signed directory.
5. **Default album:** a new MLS group at the master-key-derived `album_id` ([](/design/cryptography/keys/#key-chain)), with this device as sole admin and writer. Set `default_album_id` ([](/design/filesystem/server/#ownership-partitioning-and-quota)).
6. **Show the recovery secret** behind a **type-back gate**, which is not advisory. It is never persisted.

A second device right after signup uses the ordinary [](#cross-device-add).

## Cross-Device Add

Precondition: device A is signed in and holds the master key.

1. **Initiate on A.** This requires a **fresh local authorization** (biometric or passcode); a session token alone MUST NOT suffice. Server-side, the account must have proved a credential within **5 minutes** of the session's `authenticated_at`, which a token refresh does not reset. A stale session re-proves via `POST /v1/auth/reauthenticate`.
2. **Enrollment code.** Single-use, **10-minute** expiry, deleted on redeem or expiry, and rate-limited at redemption. The QR carries **≥64 bits**. An 8–10-digit text fallback trades entropy for transcribability. It is safe only because of the rate limit (argued in `capsule-server/src/counter/budgets.rs`) and because channel integrity never rests on the code.
3. **Channel.** Ephemeral X25519 between A and B, LAN-direct (mDNS) when available, else via server relay. It is authenticated by the code plus the DH.
4. **Verify.** Both devices show a transcript-derived safety code in the same chunked fixed-length format, **with each device's model and key fingerprint**. The user MUST explicitly confirm match and identity on **both** devices, and a mismatch aborts. This defeats a relay MITM or device swap.
5. **Transfer.** A wraps the master key under the channel key. B unwraps it, generates its own DSK + DEK, and returns their public halves.
6. **Cross-sign.** A signs B's keys with the IK, unwrapped via the master key, so any fully enrolled device can authorize an add. A then publishes the directory.
7. **Join MLS** via [](/design/cryptography/mls/#add-new-device-for-existing-member).

The server surface and the CLI path have shipped. The native add UI (QR, safety-code screens) is post-v1.

Cross-device *recovery* reuses the channel and key-transfer steps but is substitutive (devices lost) rather than additive.

## Failure Modes

| Failure | Outcome |
| --- | --- |
| Abandoned mid-flow | Code expires; nothing persisted |
| Safety codes differ | Abort |
| Stolen session token | Cannot initiate (step 1) |
| Secure element refuses keygen | Fail with actionable error; no partial state |
| Server unreachable at publish | Device keys valid locally; retry with backoff, "finishing setup" shown |
| Default-album creation fails | Account still valid; the album is re-derived and created lazily before first import |

## Validation

- First-device round-trip: escrow stored, directory has one entry, recovery secret unwraps escrow.
- Mismatched safety codes and a key-swapping relay both abort.
- Code expiry and single-use: redeem fails after expiry or reuse, and the server deletes the code either way.
- A session token without fresh local authorization cannot initiate.
- A mocked secure-element refusal surfaces an error, never partial completion.
