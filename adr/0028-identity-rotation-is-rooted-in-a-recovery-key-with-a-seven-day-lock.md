# ADR-0028 — Identity rotation is rooted in a recovery key, with a seven-day lock on changing it from a device

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Cryptography — Keys § Identity Rotation](../capsule-docs/src/content/docs/design/cryptography/keys.md#identity-rotation), [Authentication § The Identity Anchor](../capsule-docs/src/content/docs/design/authentication.md#the-identity-anchor)
- **Issues:** #1209, #1210

## Context

The User IK is the root of an account's write identity: it signs the device directory, and the
server pins its public half as the account's identity anchor. Every enrolled device holds the
master key, and the IK is sealed under the master key, so a compromised device is a compromised
IK. The only remedy the docs offered was a global account reset through an IK-signed revocation
certificate, which a user who still held the account could not use to replace a copied IK while
keeping the account, and which peers could not tell apart from a takeover.

Something a device-only attacker lacks has to authorise a new IK. The recovery secret is the one
secret no device stores: it is shown once and used only to unwrap the master-key escrow. It is
also the only user-held secret class, and the single-root invariant forbids adding another.

## Decision

A **recovery verification key**, a hybrid Ed25519 + ML-DSA-65 signing key derived from the
recovery secret (Argon2id under its own salt, then HKDF with `info = "recovery-anchor/v1"` and
the user id as salt), is pinned with the server's identity anchor on the account's first
directory publish and carried in the signed directory so peers pin it too. Rotating the IK takes
an `IdentityRotation` signed by the recovery key; the server replaces the anchor only on a valid
one, and peers follow it without a first-contact warning. Changing the recovery key itself takes
effect at once when signed by the current recovery key. Signed by the IK alone, for a user who
lost the secret but holds a device, it takes effect only after a **7-day time lock** that every
device announces and the current recovery key can cancel. The escrow is re-wrapped under the
same rule, since escrow and recovery key come from one secret. Until the user has typed the secret back during
first-device setup, the account is in an unconfirmed setup state in which a resumed setup replaces
the recovery key and escrow at once; the server records the confirmation, signed by the recovery
key the typed-back secret derives, and the lock applies from then on. Setup therefore registers
with the IK, publishes the first directory (pinning the recovery key), and only then stores the
escrow. Peers follow a recovery-key change only when the pinned recovery key signs it; an IK-only
change raises a first-contact-grade warning.

## Consequences

- A device-only attacker can start a recovery-key change but cannot finish one the user sees
  within seven days; master-key compromise gains a real IK step before the master key and album
  keys are rotated.
- No second root is added: the recovery key is a public commitment to the existing secret. Its
  public half, which the directory gives every peer, permits an offline guess test at one
  Argon2id evaluation per guess, as the escrow blob does for the server; against a secret of at
  least 128 bits that is infeasible.
- A user who loses the secret and changes it from a device must re-verify with peers, which do
  not silently follow an IK-only change.
- A user who loses the recovery secret waits seven days to set a new one from a device, which is
  the accepted cost of the lock. A user who loses both the secret and every device can still only
  reset the account.
- The recovery key in the anchor and the directory, the rotation record, the time lock and the
  unconfirmed setup state are not implemented yet; they are filed as implementation work.
