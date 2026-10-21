---
title: MLS Resilience
description: How Capsule's MLS layer recovers from lost commits, state divergence, and group corruption
status: draft
---

Recovery contracts for honest MLS failures the base protocol leaves open (steady state: [Cryptography — MLS](/design/cryptography/mls/); attacks: [Threat Model](/design/threat-model/)). Code: `capsule-core/src/crypto/authority/openmls_authority/resilience.rs`.

## Failure Modes

### Lost commit

A device's commit never reaches the server. If the device does not see its epoch on the server's chain within a detection timeout, it re-submits with backoff. Duplicates are rejected, so retries are safe. An exhausted budget is surfaced to the user, never silently dropped.

### State divergence

- **Local is behind:** replay the missed commits from the server's chain.
- **Local is ahead** (it holds a commit the server's chain lacks): treat it as a [lost commit](#lost-commit) first. The device discards its state and **re-bootstraps in full** only after the retry budget is spent, or on a *provable fork* (the server has advanced past the parent with a different commit). Partial repair is never attempted.

### Concurrent commits with the wrong ordering

When two admins rotate in the same epoch, MLS ordering decides the winner. The loser is **automatically re-proposed** against the resulting epoch, with no prompt. The invariant: one write-tier key per epoch.

### Group re-keying ceremony

A full-group re-key rotates every leaf, the AMK and the write-tier key. It runs as quiesce → commit chain → resume, and reuses the crash-resumable, `intent_id`-keyed machinery of the [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony). The album stays on the prior epoch until the ceremony completes, so two write-tier keys are never live at once. Triggers: admin, suspected compromise, or scheduled policy. If a run stalls, any owner-set member recovers the AMK lineage from the [OGK](/design/cryptography/keys/#owner-group-keys-ogks) and drives a clean epoch.

## Recovery Posture

- **The server chain is authoritative.** The server can order commits but cannot forge MLS state.
- **Re-bootstrap is always available.** Another device can remove and [re-add](/design/cryptography/mls/#add-new-device-for-existing-member) a broken device. Losing MLS state loses the in-flight ratchet, not access to data.
- **Quarantine, not silent acceptance.** Divergence is shown to the user so that bugs stay visible.

## Contract Skeleton

There is a single `reconcile_with_server` entry point, not one call per failure mode. Its `ReconcileOutcome` is one of `UpToDate`, `Reconciled`, `Diverged` (needs user action) or `Unrecoverable` (needs re-bootstrap). Re-keying is `rekey_group`. Both are defined in `resilience.rs`.

## Validation

- **Lost commit (smoke):** a network failure during a commit; the retry succeeds without producing a duplicate epoch.
- **Divergence (unit):** state that disagrees with a mocked chain is detected, and reconciliation reaches the server's state.
- **Concurrent rotation (smoke):** the two rotations serialize, and one replays on top of the other.
- **Re-key atomicity (smoke):** a crash mid-ceremony resumes on restart.
