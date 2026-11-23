# ADR-0025 — A blob root has exactly one serving process, enforced by a lock

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Server Filesystem — Blob Root Ownership](../capsule-docs/src/content/docs/design/filesystem/server.md#blob-root-ownership)
- **Issues:** #1243, #1213, #1214

## Context

The upload path keeps four kinds of state in the serving process, not in PostgreSQL:

- the in-flight range claims (`capsule-server/src/upload/chunk.rs:29-32`);
- the finalize claim, which has no lease. Boot recovery re-drives every `WaitingForProcessing` session it finds, on the stated assumption that "a deployment runs one process over its blob root" (`capsule-server/src/upload/finalize.rs:111-115`, `capsule-server/src/cli.rs:684-727`);
- the two per-chunk rate budgets, counted in process by `SplitCounters` (`capsule-server/src/counter/split.rs:9-34`, ADR-0015);
- the stage itself: a chunk can only be written by the process that created the stage (`NotStaged`, `capsule-server/src/blob/fs.rs:370-381`).

Nothing enforced the assumption. `FilesystemBlobStore::open` takes no lock and writes no pid file (`capsule-server/src/blob/fs.rs:170-189`); the only `flock` in the server is the owner ledger's (`capsule-server/src/ledger/fs.rs:364-373`). The self-hosting guide told operators not to run two `serve` processes on one tree, and the design audits (#1213 item 4, #1214) found other docs that already anticipated replicas (share-link revocation caching, federation pull leases). A second replica booting on a shared root would have taken over every claim its sibling was mid-way through, including ones whose commit step had already run.

With Valkey retired (ADR-0015), every other port is in PostgreSQL and replica-safe, so the blob root is the last piece of single-process state.

## Decision

Exactly one `serve` process owns a blob root. At boot, before it lists sessions or recovers a claim, it takes an exclusive lock on `{blob_root}/.server/lock` and holds it for its lifetime; a second `serve` that finds the lock held refuses to start. `rebuild --apply` takes the same lock. Scaling is vertical first, then PostgreSQL read replicas for the sync feed. The blob root stays a POSIX filesystem; object storage is out of v1.

## Consequences

- The no-lease finalize recovery is sound by construction rather than by documentation.
- The lock relies on the blob root's filesystem sharing advisory locks between every process and host that writes it, which is already a stated requirement of the filesystem contract.
- Horizontal upload scaling is foreclosed for v1. A future multi-process design needs, at least: a root per process with requests routed to the owner by upload id (sticky routing at the proxy, or a redirect); the two chunk budgets moved to PostgreSQL or pinned to the owner; and a claim lease with a fencing token, so a slow former holder's terminal write cannot race a new holder's. A multi-replica test in `capsule-e2e` gates it.
- An operator who runs two `serve` processes by mistake gets a refusal at boot instead of silent double finalization.

## Considered and rejected

- **Shared POSIX root across replicas (NFS).** Every replica would need the stage, the claims and the budgets shared, which is the multi-process design above plus the network filesystem's weaker rename and lock semantics. It buys nothing a single larger instance does not, at the scale v1 targets.
- **Object storage as the blob root.** An object store has no rename and no lock. Its multipart upload needs parts of at least 5 MiB, which the adaptive, out-of-order chunk protocol cannot map onto, so staging would stay on a local filesystem anyway, and the owner ledger would need conditional-write segments or a move into PostgreSQL. The requirements for a future adapter are recorded in the contract.
- **A claim lease without fencing.** A lease lets a second finalizer take over a claim whose holder is only slow, and nothing then orders the two terminal writes (`Completed` from one, `FailedProcessing` from the other).
- **Keep the assumption documented but unenforced.** The audits showed the documentation was already drifting toward replicas; an unenforced assumption is the one an operator breaks first.
