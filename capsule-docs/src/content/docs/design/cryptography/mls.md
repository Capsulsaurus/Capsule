---
title: MLS Group Membership
description: How Capsule binds MLS (RFC 9420) to its identity layer and uses it for album membership
audit: none
---

Album membership is MLS (RFC 9420) over OpenMLS with the X-Wing ciphersuite `0x004D` ([](/design/cryptography/primitives/#mls-ciphersuite)). `OpenMlsAuthority` (`capsule-core/src/crypto/authority/openmls_authority/`) implements `AlbumAuthority` ([](/design/cryptography/keys/#write-authority-interface)); `ReferenceAuthority` is the deterministic fixture.

- **A private codepoint is fine** because interop is a non-goal. A ratified suite is a re-pin via the upgrade ceremony ([](/design/versioning/#album-upgrade-ceremony)) and `crypto_suite_id` ([](/design/cryptography/primitives/#versioning-identifiers)).
- **ChaCha20-Poly1305** ([](/design/cryptography/primitives/#mls-control-aead)) protects only control traffic. User data uses AES-GCM ([](/design/cryptography/primitives/#bulk-aead)).
- **PQ authentication lives at the identity layer.** LeafNodes are Ed25519-only, so identity certificates sign the MLS key with **both** halves of the hybrid signature ([](/design/cryptography/primitives/#signature-scheme)). Peers MUST verify both before admitting a leaf. The leaf key itself is a software key, since no secure element signs Ed25519 ([](/design/cryptography/keys/#device-keys)).
- **Blocking a user** removes all their devices in one `Commit`, so one epoch bump.

## Membership Operations

Every ceremony is idempotent ([](/design/threat-model/validation/#idempotency-invariants)). Who may commit which ceremony, and who may send which key delivery, is owned by [](/design/cryptography/keys/#key-delivery-and-who-may-send-it); members reject a commit that breaks it.

### Add user Bob to album

The owner commits it.

1. Fetch Bob's KeyPackages from the device directory ([](/design/cryptography/keys/#device-directory)).
2. `Add` + `Commit` with all of his devices as leaves.
3. Bob exports the new epoch's AMK from his `Welcome`. The committer sends him the older AMKs the album's history policy ([](#history-delivery-for-new-joiners)) allows, and, if Bob is a writer, the epoch's write-tier key.

### Remove user Charlie

The owner commits it.

1. `Remove` + `Commit` for all of Charlie's devices. The new MLS epoch excludes him.
2. Every remaining member exports the new epoch's AMK, which Charlie cannot compute. The committer mints the epoch's write-tier key and seals it to the remaining writers. All later uploads use the new epoch.
3. Charlie keeps the AMKs he held: past access cannot be undone.

### Add new device for existing member

If Alice is the owner or a writer, one of her existing devices commits the new leaf; if she is a reader, her device proposes it and an owner or writer commits. The new device exports the current AMK from its `Welcome`, and the committer's history bundle carries every older AMK Alice is entitled to. First-device enrollment is in [](/design/device-enrollment/).

### Remove lost device

One of the user's remaining devices commits the `Remove` (or, for a reader, proposes it for an owner or writer to commit). As with a user removal, the epoch MUST advance, because the lost device's keys are presumed compromised. A lost device held the account master key, so the account-level response is [](/design/cryptography/keys/#master-key-compromise).

## History Delivery for New Joiners

A member that processed an epoch's commit, or joined at it, exports that epoch's AMK itself ([](/design/cryptography/keys/#where-an-amk-comes-from)). Everything else rides four MLS application messages, canonical CBOR, whose sender rules are in [](/design/cryptography/keys/#key-delivery-and-who-may-send-it):

- `AlbumKeyDistribution { amk_version, amk_bytes }`: a receipt for the current epoch, compared against the receiver's own export.
- `WriteTierDistribution { amk_version, write_tier_seed }`: the epoch's write-tier private half, sealed to each writer.
- `AlbumHistoryBundle`: a joiner's prior epochs, each with its AMK and attested write-tier public key, never a private half.
- A signed upgrade intent ([](/design/versioning/#album-upgrade-ceremony)), processed by the upgrade path and refused by key delivery.

Key management owns the AMK ([](/design/cryptography/keys/#album-master-keys-amks)) and encryption consumes it ([](/design/cryptography/encryption/#asset-key-derivation)).

- **`history_policy` is fixed per album, not per add.** It is set at creation: `Full` (every AMK since v1, the default for shared albums) or `Capped(n)` (only recent epochs; older assets show placeholders). A user's readable range therefore never depends on who added them. Changing the policy requires an album upgrade ceremony ([](/design/versioning/#album-upgrade-ceremony)).
- **Epoch ceiling on join.** The joiner takes the highest `amk_version` attested by the Welcome's commit chain as its monotonic ceiling and rejects any manifest claiming a higher epoch, so it never trusts the server's counter ([](/design/cryptography/keys/#write-authorization)).

## Comments and Reactions

Members of an album may comment on an asset or on the album and react to an asset. Each is end-to-end encrypted, authenticated by MLS and invisible to the server.

```rust
AlbumActivity {
  activity_id: UUIDv7,
  target:      Album | Asset(asset_id),
  kind:        Comment { text: String /* ≤ 4096 bytes */ } | Reaction { emoji: String /* one grapheme */ },
  replaces:    Option<activity_id>,   // an edit, by the original author only
  removes:     Option<activity_id>,   // a delete, by the author or the album owner
  _unknown:    Map,
}
```

- **Sent as an MLS application message** of the current epoch, so the sender is authenticated by the group and only current members can read it. Any member, reader or writer, may send one.
- **Kept as a sealed record.** The sender also posts the same record sealed under the epoch's AMK, in the metadata-blob format ([](/design/cryptography/encryption/#metadata-blob-wire-format)), so a new device or a joiner reads past activity within the album's `history_policy` ([](#history-delivery-for-new-joiners)), and a removed member reads none from later epochs.
- **Order.** Activity is ordered by epoch, then by the sender's sequence within it, then by `activity_id`. Edits and deletes are later records, so replay is idempotent.
- **Moderation.** The owner may remove any comment; a removal hides it on every member's device. Reporting to the server operator follows [](/design/moderation/).
- **Alerts are local.** A member's device raises a local alert after it syncs new activity ([](/design/notifications/#alert-classes)); no server composes or relays one.
- **Not on share links.** A share-link viewer has no group membership and can neither read nor write activity.

## Forward Secrecy & Post-Compromise Security

At-rest ciphertext persists, so true forward secrecy does not apply to it. Each epoch bump guarantees that compromising the current epoch reveals nothing about past epochs and that removed members cannot read future epochs. Forward secrecy for data in transit comes from TLS 1.3 ([](/design/networking/#transport-security)). Lost commits, divergence and re-keying are in [](/design/mls-resilience/).

## Known Gaps

- Album activity is not implemented: no `AlbumActivity` message, record or alert exists.

## Validation

- **Round trip:** add user, add device, remove user, remove device and AMK rotation, with all members' views agreeing after each commit.
- **Welcome:** `Full` delivers every prior AMK and decrypts every prior asset; `Capped(n)` delivers exactly the last _n_.
- **Policy consistency:** the same user added through different devices or in different orders receives an identical AMK range.
- **Epoch ceiling:** a manifest claiming an epoch above the chain's attested ceiling is rejected.
- **Idempotency:** a replayed commit is rejected and leaves state unchanged.
- **Joiner trust:** a history bundle or write-tier delivery from any leaf other than the Welcome's GroupInfo signer is rejected, even when it arrives first.
- **Identity binding:** a LeafNode key without an ML-DSA identity signature is rejected.
- **Concurrent commits (smoke):** parallel proposers converge without splitting the group.

End-to-end cases are in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
