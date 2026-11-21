# ADR-0032 — Trusted contacts recover an account after a notice period the owner can veto

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Backup and Recovery § Opt-in: Trusted-Contact Recovery](../capsule-docs/src/content/docs/design/backup-recovery.md#opt-in-trusted-contact-recovery)
- **Issues:** #1296

## Context

Every recovery path assumed the owner could still act: they hold the recovery secret, a
surviving device, a backup artifact, or enough Shamir shares. None covered the case the owner
cannot act at all, which for a lifetime photo library is the case that matters most to a
family: incapacity or death. With end-to-end encryption, nobody else can recover the library,
not even the server operator.

Prior art: an end-to-end encrypted photo service lets a user name a trusted contact who can
start recovery; the owner is notified and can block it within 7, 14 or 30 days, after which the
contact resets the account. A later "legacy kit" splits a secret into printed 2-of-3 cards,
again behind a server-enforced notice period the owner can block.

Opt-in Shamir already existed here, but a quorum of shares recovers immediately: there is no
window in which the owner can say no.

## Decision

A random key `K_tc` seals the recovery seed. `K_tc` is split between a server half and an
`m`-of-`n` Shamir split across contacts (or printed cards). A contact's signed claim starts a
notice period of 7, 14 or 30 days. Every owner device alerts locally and may block it. An
unblocked claim releases the server half, and the contact recovers on a new device.

## Consequences

- Neither the server nor any set of contacts can recover alone, and the server's half buys the
  owner the notice period.
- The path is a wrap of the existing seed, so the single-root invariant holds and rotating the
  recovery secret invalidates every share.
- The release is a server-side time comparison, a new class of fault the risk register tracks
  (R21).
- A contact must be a registered user so a share can be sealed to an identity key; printed
  cards cover people without one.

## Considered and rejected

- **Plain Shamir across contacts.** No notice period: a colluding quorum recovers at once.
- **Server-only escrow.** It would make the operator able to recover any account, which the
  threat model forbids.
