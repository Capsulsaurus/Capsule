---
title: Device Enrollment
description: First-device bootstrap and cross-device add ceremonies for Capsule accounts
status: draft
---

Each device holds a DSK + DEK cross-signed into the user's device directory ([](/design/cryptography/keys/#device-directory)). Two ceremonies put a device there. Recovery after losing every device is cross-device recovery ([](/design/backup-recovery/#default-mechanisms)), not enrollment.

**Trust split:** account-creation auth ([](/design/authentication/)) establishes *who owns the account* and confers no data access. Data authority is cryptographic: the master key and device keys verified against the directory.

## First-Device Enrollment

Keys are made before anything is sent, and each server step needs the one before it: registration pins the IK, the first publish verifies under it and pins the recovery key, and the escrow stores are signed by keys those two steps pinned.

1. **Master key** from the OS CSPRNG ([](/design/cryptography/primitives/#randomness)), shaped per [](/design/cryptography/keys/#registered-accounts), and the **recovery secret** it will be escrowed under.
2. **User IK** (composition: [](/design/cryptography/keys/#user-identity-keys-user-iks)): random, never derived from the master key, with its public halves in the directory. Beside it, the **recovery verification key** derived from the recovery secret ([](/design/cryptography/keys/#identity-rotation)), whose public half the directory also carries.
3. **Device keys** (composition: [](/design/cryptography/keys/#device-keys)), signed by the IK. The classical half MUST be non-exportable in the secure element where one exists. Secure elements offer only P-256, so the hardware path is the planned P-256 hybrid with the PQ half software-sealed under hardware-protected material. Until then both halves are software. The master key, the IK and the device keys are persisted in the device's secure storage **before** anything is sent, so a crash after the next step resumes rather than orphaning the anchor.
4. **Register** with the address, the password, the IK public key and its proof of possession, which pins the identity anchor ([](/design/authentication/#the-identity-anchor)). On an OIDC deployment the device signs in through the provider instead, and step 5 pins the anchor.
5. **Publish** the IK-signed directory listing this device. It verifies under the anchor and pins the recovery verification key beside it. The account can now write.
6. **Escrow:** the master key wrapped under the recovery secret, stored as the master-key escrow under a signature by the recovery key step 5 pinned ([](/design/backup-recovery/#master-key-escrow)), and the IK sealed under the master key, stored as the sealed identity under a signature by this device's DSK ([](/design/cryptography/keys/#registered-accounts)). Both ride the session registration just opened, which is fresh ([](/design/authorization/#freshness-gated-operations)).
7. **Default album:** a new MLS group at the master-key-derived `album_id` ([](/design/cryptography/keys/#key-chain)), with this device as sole admin and writer. Set `default_album_id` ([](/design/filesystem/server/#ownership-partitioning-and-quota)).
8. **Show the recovery secret** behind a **type-back gate**, which is not advisory. The typed-back secret re-derives the recovery key, which signs a confirmation the server records; that ends [](#unconfirmed-setup).

### Unconfirmed Setup

From the first publish until the type-back confirmation, the account is in **unconfirmed setup**. The recovery secret is never persisted, and until the type-back no one is known to hold it, so the recovery-key time lock ([](/design/cryptography/keys/#identity-rotation)) would protect nothing.

- **The recovery key and the master-key escrow are replaced freely.** A resumed setup generates a new recovery secret and recovery key, republishes the directory with the new key under the IK, and stores a new escrow signed by the new key. The server accepts that change at once, without the 7-day lock.
- **Confirmation is one-way.** The server records it once, and from then on every recovery-key and escrow change follows the time-lock rule.
- **Unconfirmed setup is bounded.** Lock-free replacement is accepted only within 7 days of the first publish. After that the server treats the account as confirmed for the time-lock rule, and the app keeps prompting for the type-back, so an abandoned setup cannot keep the lock-free path open indefinitely.
- **Nothing is shared before confirmation.** The device does not invite, share or accept an invitation while setup is unconfirmed, so no peer has pinned a recovery key that may still be replaced. The app shows "finishing setup" until confirmation.

A second device right after signup uses the ordinary [](#cross-device-add).

## Cross-Device Add

Precondition: device A is signed in and holds the master key.

1. **Initiate on A.** This requires a **fresh local authorization** (biometric or passcode); a session token alone MUST NOT suffice. Server-side, issuing the code is a freshness-gated operation ([](/design/authorization/#freshness-gated-operations)). A stale session re-proves with its password, or through the identity provider on an OIDC deployment.
2. **Enrollment code.** Single-use, **10-minute** expiry, deleted on redeem or expiry. The QR carries **≥64 bits**. An **8-digit** text fallback trades entropy for transcribability. The fallback is safe for two reasons. Redemption is rate-limited ([](/design/authorization/#rate-limits)): every fallback guess from every source spends one deployment-wide budget, so at most about 330 guesses reach any one code in its life, a chance of about 3 in a million. And channel integrity never rests on the code. Redemption is charged per source first, so one caller cannot spend the deployment-wide fallback budget alone. The limiter **fails closed**: an unreachable counter store refuses the redemption, and never admits it unbudgeted.
3. **Channel.** Ephemeral X25519 between A and B, LAN-direct (mDNS) when available, else via server relay. It is authenticated by the code plus the DH. The relay is a dumb pipe gated by its unguessable channel handle. It is budgeted per channel, and each direction's mailbox holds a bounded number of payloads, so a leaked handle cannot fill the server's disk.
4. **Verify.** Both devices show a transcript-derived safety code in the same chunked fixed-length format, **with each device's model and key fingerprint**. The user MUST explicitly confirm match and identity on **both** devices, and a mismatch aborts. This defeats a relay MITM or device swap.
5. **Transfer.** A wraps the master key under the channel key. B unwraps it, generates its own DSK + DEK, and returns their public halves.
6. **Cross-sign.** A signs B's keys with the IK, unwrapped via the master key, so any fully enrolled device can authorize an add. A then publishes the directory.
7. **Join MLS** via [](/design/cryptography/mls/#add-new-device-for-existing-member). In each album where the account is a reader, A only proposes B's leaf; an owner or writer commits it, so B can read that album once one of them next syncs.

The server surface and the CLI path have shipped. The native add UI (QR, safety-code screens) is post-v1.

Cross-device *recovery* reuses the channel and key-transfer steps but is substitutive (devices lost) rather than additive.

## Failure Modes

| Failure | Outcome |
| --- | --- |
| Abandoned mid-flow | Code expires; nothing persisted |
| Safety codes differ | Abort |
| Stolen session token | Cannot initiate (step 1) |
| Counter store unreachable at redemption | Refused (`503`); B retries after `Retry-After`. Never admitted unbudgeted |
| Fallback budget spent by a flood | The typed code waits for the next minute; scanning the QR still works |
| Secure element refuses keygen | Fail with actionable error; no partial state |
| Crash between registering and publishing | The persisted keys resume setup ("finishing setup") under the same anchor |
| Crash before the type-back confirmation | Setup is unconfirmed: a new recovery secret is generated, and it replaces the pinned recovery key and any stored escrow without the time lock ([](#unconfirmed-setup)) |
| Server unreachable at publish | Device keys valid locally; retry with backoff, "finishing setup" shown |
| Never published | The account holds no data and is swept after its window ([](/design/authentication/#the-identity-anchor)); the address can register again |
| Default-album creation fails | Account still valid; the album is re-derived and created lazily before first import |

## Validation

- First-device round-trip: registration pins the anchor, the directory has one entry and verifies under the anchor, the escrow store verifies under the pinned recovery key, the recovery secret unwraps the escrow, and the type-back confirmation is recorded.
- A crash injected after registration resumes from the persisted keys and publishes under the same anchor.
- A crash injected after the escrow store and before the type-back resumes with a new secret, replacing the recovery key and escrow at once; after confirmation the same replacement signed by the IK alone waits out the time lock.
- Mismatched safety codes and a key-swapping relay both abort.
- Code expiry and single-use: redeem fails after expiry or reuse, and the server deletes the code either way.
- A session token without fresh local authorization cannot initiate.
- Redemption: a per-source flood is refused before it reaches the deployment-wide fallback budget; an unreachable counter store refuses; a relay mailbox past its bound refuses the append.
- A mocked secure-element refusal surfaces an error, never partial completion.
