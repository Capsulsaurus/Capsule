---
title: MLS Resilience
description: How Capsule's MLS layer recovers from lost commits, state divergence, and group corruption
---

OpenMLS handles MLS (RFC 9420) correctly under normal operation — commits ordered by the group's chain, duplicates rejected, ratchet advanced atomically. But MLS can still hit scenarios the base protocol does not resolve on its own: a commit lost in transit, two devices proposing concurrently with the wrong ordering, a member whose local state has diverged from the server's. This doc owns Capsule's recovery contracts for those edge cases.

It is kept **separate** from [Cryptography — MLS](/design/cryptography/mls/) (which owns the ciphersuite binding and the four standard ceremonies) because recovery is a distinct, cross-cutting concern — it reaches into the [OGK](/design/cryptography/keys/#owner-group-keys-ogks), backup, and quarantine UX, not the steady-state membership protocol. The recovery surfaces here are exercised in `capsule-core::crypto::mls` (the OpenMLS wrapper) and surface to users through quarantine + reconciliation UX in the native clients.

## Failure Modes

The MLS-layer scenarios that need defined recovery contracts. Each is a candidate damage scenario that the existing [scenario map](/design/threat-model/scenarios/#damage-scenario--invariant-map) does not currently address head-on:

### Lost commit

A device sends an MLS commit (e.g. an `Add` or AMK rotation) and the server never receives or persists it. The sending device believes it succeeded; other devices never see the new epoch.

**Recovery direction:** the server's MLS commit chain is the source of truth. A device that doesn't see its committed epoch reflected in the chain within a detection timeout (default 30 s) treats the commit as lost and re-submits, backing off on each attempt (default 30 s → 2 min → 10 min). The commit chain provides idempotency — OpenMLS rejects a duplicate, so a retry that *did* land is harmless. After the backoff budget is exhausted (default 3 attempts) the membership change is surfaced to the user ("couldn't sync — will retry when connectivity returns"), never silently abandoned.

### State divergence

Two devices' local MLS state has diverged — different views of the group's current epoch, different write-tier key, different member list. This can happen after a buggy commit, an incomplete sync, or a long offline period.

**Recovery direction:** the device with the older epoch reconciles by replaying every commit it missed from the server's chain. A device whose local state is *ahead* of the server — it holds a commit whose hash is **absent from the server's authoritative chain** (a local state-mutation bug, or a commit the server never persisted) — declares itself unreconcilable, discards its local group state, and **re-bootstraps in full**. Partial re-bootstrap is deliberately not attempted: MLS group state is small, so a clean full re-fetch is simpler to reason about than splicing suspect local state, and is the only path taken.

### Concurrent commits with the wrong ordering

OpenMLS handles ordinary concurrent commits — one wins, the other re-proposes. But a *concurrent AMK rotation* where two admins both rotate at the same epoch needs care: the second commit must observe the first's new write-tier key in its proposal envelope, or the resulting epoch carries two write-tier keys.

**Recovery direction:** MLS's commit ordering serializes the two rotations; the losing rotation is **automatically re-proposed** against the resulting epoch — no user confirmation. The replay is deterministic and idempotent (it re-runs against fresh state and converges on one write-tier key per epoch), so prompting an admin on every concurrent rotation would add friction without adding safety.

### Group re-keying ceremony

A scheduled or admin-triggered re-keying of the entire album group (every member's leaf rotates; fresh AMK; fresh write-tier key). This is more invasive than a single member add/remove and may be needed periodically for long-lived albums or after a suspected compromise.

**Recovery direction:** re-keying runs as a quiesce → commit-chain → resume ceremony, modeled on the [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony) and sharing its crash-resume machinery (an `intent_id`-keyed, idempotent, resumable sequence). Every member's client processes the leaf-update chain as one logical operation; until it completes the album stays on the prior epoch, so a partial run never leaves two live write-tier keys. **Triggers:** admin-initiated, automatic after a suspected compromise, and optional scheduled rotation for long-lived albums (deployment policy). **The [OGK](/design/cryptography/keys/#owner-group-keys-ogks) is the recovery anchor:** if a re-keying stalls partway, any current owner-set member recovers the album's AMK lineage from the OGK escrow and re-drives a fresh, clean epoch out-of-band — the ceremony can always be completed or restarted without data loss.

## Recovery Posture

Across the failure modes above, Capsule's recovery posture is consistent:

- **Server chain is authoritative.** Any local state inconsistency is reconciled by replaying the server's chain. The server cannot *forge* MLS state (it holds no MLS group secrets) but it can *order* commits.
- **Re-bootstrap is always available.** A device whose MLS state is unrecoverable can be removed and re-added by another device (the standard "Add new device" flow from [Cryptography — MLS](/design/cryptography/mls/#add-new-device-for-existing-member)). This is the bottom-of-stack recovery — losing local MLS state never loses access to the data, just to the in-flight ratchet.
- **Quarantine, not silent acceptance.** A device that detects local-vs-server state divergence surfaces it to the user (not silently re-bootstraps), so a divergence caused by a bug is visible and investigable.

## Contract Skeleton

Reconciliation is a **single entry-point**, not per-failure-mode calls: the caller asks "bring me current" and the outcome enum reports what happened, including the two cases that escalate to user action or re-bootstrap.

```rust
// in capsule-core::crypto::mls
enum ReconcileOutcome {
    UpToDate,
    Reconciled { applied_commits: Vec<CommitHash> },
    Diverged { local_epoch: u64, server_epoch: u64 },  // requires user action
    Unrecoverable,  // requires re-bootstrap
}

fn reconcile_with_server(group: GroupId) -> Result<ReconcileOutcome, MlsError>;
fn rekey_group(group: GroupId, reason: RekeyReason) -> Result<(), MlsError>;
```

## Validation

- **Lost-commit recovery (smoke).** Inject a network failure during a commit; assert the sending device's retry succeeds; assert idempotency (no duplicate epoch).
- **State-divergence detection (unit).** Construct a local MLS state that disagrees with a mocked server chain; assert detection; assert reconciliation produces the server-authoritative state.
- **Concurrent rotation (smoke).** Two admins rotate the same epoch; assert serialization; assert one rotation replays against the other's result.
- **Re-keying atomicity (smoke).** Inject a crash mid-rekey; assert the ceremony resumes on restart (similar to the [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony) idempotency).

The relationship to [Threat Model](/design/threat-model/) is that several scenarios in the existing map (e.g. row #16 "attacker with all current keys") are upstream of this doc — MLS resilience is about recovering from honest failure, not adversarial attack. The two combine cleanly because both routes ultimately reduce to "re-bootstrap from a higher recovery path."
