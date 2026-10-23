# Roadmap

The path to the first release the owner trusts with irreplaceable data: **the CLI/TUI imports,
the server stores permanently, and every step is covered by automated tests.** Everything else
waits behind it. Work is tracked as GitHub issues in the
[MVP: dogfood](https://github.com/Capsulsaurus/Capsule/milestone/2) milestone; each issue names
what blocks it.

## A. Format freeze

Last-chance changes to persisted and signed formats. They land first because nothing after the
freeze may need a migration.

- [#665](https://github.com/Capsulsaurus/Capsule/issues/665) Version the provenance envelope; preserve unknown manifest fields
- [#666](https://github.com/Capsulsaurus/Capsule/issues/666) Bounded-memory (segmented) encryption for originals
- [#667](https://github.com/Capsulsaurus/Capsule/issues/667) Labels on stacks, albums and image regions
- [#668](https://github.com/Capsulsaurus/Capsule/issues/668) Keep original capture time and location beside corrections; location editing
- [#669](https://github.com/Capsulsaurus/Capsule/issues/669) Decide the major dependency bumps

## B. Transfer hardening

Closing every path by which an original could be lost.

- [#670](https://github.com/Capsulsaurus/Capsule/issues/670) `push` verifies custody, persists receipts, continues past failures
- [#671](https://github.com/Capsulsaurus/Capsule/issues/671) `import --move` releases a source only on server custody
- [#673](https://github.com/Capsulsaurus/Capsule/issues/673) `capsule restore` with byte-for-byte verification
- [#741](https://github.com/Capsulsaurus/Capsule/issues/741) Album keys escrowed under the master key, so a recovered device restores its albums
- [#674](https://github.com/Capsulsaurus/Capsule/issues/674) One protocol-version source; honour the window and minimum build everywhere
- [#675](https://github.com/Capsulsaurus/Capsule/issues/675) SDK transfer event stream
- [#676](https://github.com/Capsulsaurus/Capsule/issues/676) Backup and restore drill for a durable deployment
- [#693](https://github.com/Capsulsaurus/Capsule/issues/693) Custody receipt written in the finalize transaction
- [#694](https://github.com/Capsulsaurus/Capsule/issues/694) Upload error paths hardened against broken and hostile clients
- [#695](https://github.com/Capsulsaurus/Capsule/issues/695) Every original stored and restored byte-exact, whatever the codecs support
- [#696](https://github.com/Capsulsaurus/Capsule/issues/696) Per-owner ledger in the blob store
- [#697](https://github.com/Capsulsaurus/Capsule/issues/697) `capsule-server rebuild`, sync epoch and client re-announce
- [#704](https://github.com/Capsulsaurus/Capsule/issues/704) A trusted client address; per-source-IP limits on share serve and drop creation
- [#706](https://github.com/Capsulsaurus/Capsule/issues/706) Signed storage attestation for the durability verdict
- [#707](https://github.com/Capsulsaurus/Capsule/issues/707) Client cache budget driven by the connection class
- [#754](https://github.com/Capsulsaurus/Capsule/issues/754) Connection-class cache budget wired into the client sweep and bulk scheduling
- Also: [#409](https://github.com/Capsulsaurus/Capsule/issues/409), [#440](https://github.com/Capsulsaurus/Capsule/issues/440), [#452](https://github.com/Capsulsaurus/Capsule/issues/452), [#621](https://github.com/Capsulsaurus/Capsule/issues/621), [#646](https://github.com/Capsulsaurus/Capsule/issues/646)

## R. Restructure

Before the TUI builds on it.

- [#698](https://github.com/Capsulsaurus/Capsule/issues/698) Split a stateless `capsule-core` from a new `capsule-library` crate
- [#699](https://github.com/Capsulsaurus/Capsule/issues/699) Unit tests in sibling `tests.rs` files, enforced

## P. Peering

- [#700](https://github.com/Capsulsaurus/Capsule/issues/700) Peering hardening: anti-rollback, revocation, version window, no resurrection
- [#701](https://github.com/Capsulsaurus/Capsule/issues/701) Upload delegation between a user's own devices

## V. Validation

Automated and deterministic, per the [testing plan](capsule-docs/src/content/docs/development/testing.md).

- [#677](https://github.com/Capsulsaurus/Capsule/issues/677) Property, fuzz, mutation and snapshot harnesses
- [#678](https://github.com/Capsulsaurus/Capsule/issues/678) Kill-anywhere end-to-end suite
- [#679](https://github.com/Capsulsaurus/Capsule/issues/679) Golden wire fixtures per protocol version

## C. The TUI

The CLI becomes a full-screen TUI on a terminal while every subcommand stays scriptable.

- [#680](https://github.com/Capsulsaurus/Capsule/issues/680) English-only CLI
- [#681](https://github.com/Capsulsaurus/Capsule/issues/681) TUI foundation
- [#682](https://github.com/Capsulsaurus/Capsule/issues/682) Library browser
- [#683](https://github.com/Capsulsaurus/Capsule/issues/683) Import and push flows
- [#684](https://github.com/Capsulsaurus/Capsule/issues/684) Real-time transfer visualization
- [#685](https://github.com/Capsulsaurus/Capsule/issues/685) Label editor and metadata repair
- [#686](https://github.com/Capsulsaurus/Capsule/issues/686) Sync, restore, share and auth screens

## D. Dogfood gate

- [#687](https://github.com/Capsulsaurus/Capsule/issues/687) The checklist that must pass before real data goes in

## After the MVP

Media follows Rawshift: [#702](https://github.com/Capsulsaurus/Capsule/issues/702) adopts
everything it supports and deletes Capsule's duplicates once a Rawshift release covers video,
resizing, metadata extraction and hardware acceleration. Until then every original is still kept
([#695](https://github.com/Capsulsaurus/Capsule/issues/695)).

iPhone and Android phone, then tablets, then macOS ([Clients](capsule-docs/src/content/docs/design/clients.md)),
then federation. Deferred work carries the `track:deferred` label and is not drained
automatically, for example porting the TUI's transfer visualization and label editor to the apps
([#689](https://github.com/Capsulsaurus/Capsule/issues/689)) and the apps' responsiveness benchmark
([#703](https://github.com/Capsulsaurus/Capsule/issues/703)).

## History

The slice ledger (`SLICES.md`) is retired and its final version is archived at
[`a0bc92c2:SLICES.md`](https://github.com/Capsulsaurus/Capsule/blob/a0bc92c2/SLICES.md). Code
comments state the behaviour they uphold and carry no slice ids.
