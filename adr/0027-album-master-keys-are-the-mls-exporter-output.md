# ADR-0027 — Album master keys are the MLS exporter output of their epoch

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Cryptography — Keys § Where an AMK Comes From](../capsule-docs/src/content/docs/design/cryptography/keys.md#where-an-amk-comes-from), [§ Key Delivery and Who May Send It](../capsule-docs/src/content/docs/design/cryptography/keys.md#key-delivery-and-who-may-send-it)
- **Issues:** #1209

## Context

The key hierarchy said an AMK was "random 32 B per epoch, distributed over MLS application
messages", and that "AMKs are never derived from MLS ratchet state, which is ephemeral". The
rule protected the backup path: a key that existed only inside the ratchet would be lost with
the ratchet, and the invariant is that losing every device but holding the recovery secret
restores every photo.

The code had already left that rule. `OpenMlsAuthority::export_current_amk`
(`capsule-core/src/crypto/authority/openmls_authority/mod.rs`, label `AMK_EXPORT_LABEL =
"capsule amk v1"`, album id as the exporter context) computes the AMK with the RFC 9420
exporter, and the `AlbumKeyDistribution` it then broadcasts only tells the others to mark the
epoch present. The design doc and the code therefore disagreed about where the secret comes
from. The distribution path also had defects of its own: `process_key_delivery` applies a
key-delivery message from any member, so any member could win the race to deliver an epoch's
key; and the writer filter returns every member, so readers received the write-tier private
half.

A random, distributed AMK needs a rule for who may send it and what a receiver does with two
different keys for one epoch. An exported AMK needs neither for members present at the epoch:
each computes the same value from the key schedule MLS has already agreed and authenticated,
and a removed member never sees it. The rule's real concern, durability, does not depend on how
the key is made but on whether it is stored before it is needed.

## Decision

An album's AMK for an epoch is `MLS-Exporter("capsule amk v1", context = album_id, 32)`,
computed by each member when it processes the commit that opens the epoch, and written to the
album keystore and the album escrow before any blob is sealed under it. It is never exported
twice and never re-derived; the backup path reads stored AMKs only, so it stays independent of
the ratchet. Key delivery is narrowed to what a member cannot export itself: a receiver compares
any delivered AMK for an epoch it exported with its own export and rejects a mismatch, the
first valid delivery wins and a conflicting one is surfaced, only the committer of an epoch may
deliver its write-tier key (sealed per writer device), and only the committer of a joiner's Add
may deliver its history bundle. An album under the offline `ReferenceAuthority` has no group,
so its owner still mints each AMK at random.

## Consequences

- The rule "AMKs are never derived from MLS state" is withdrawn by owner decision. Durability
  moves from the derivation to the ordering: escrow before first use is now a hard requirement,
  because an epoch's secret cannot be exported again once MLS advances.
- `AlbumKeyDistribution` loses its authority. It remains as a receipt that is checked against
  the receiver's own export, which closes the delivery race for current members.
- Prior-epoch AMKs a joiner receives in a history bundle cannot be checked by export. The sender
  rule and the key commitment of the first blob opened under them
  ([ADR-0017](0017-every-encrypted-blob-carries-a-header-with-a-key-commitment.md)) turn a forged
  one into a *wrong key*, never a silent misread.
- The sender checks, the per-writer HPKE seal of the write-tier key and the role model that
  decides who may commit are not implemented yet; they are filed as implementation work, and
  production albums still run `ReferenceAuthority`.
