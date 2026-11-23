# ADR-0029 — Custody receipts are chained per owner

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Durability § Custody Receipts](../capsule-docs/src/content/docs/design/transfer/durability.md#custody-receipts)
- **Issues:** #1242

## Context

Every custody receipt on a server extended one server-wide hash chain: `receipt_seq` was strictly
monotonic per server and `prior_receipt_hash` named the previous receipt of any owner
(`capsule-server/src/attestation/mod.rs`). To keep that chain from forking, the finalization
transaction took `LOCK TABLE custody_receipts IN SHARE ROW EXCLUSIVE MODE` before allocating a
position (`capsule-server/src/attestation/postgres.rs`), so every finalization on the server,
for every account, waited on one lock.

The chain bought little for that cost. Receipts are fetched owner-only, so the only party that
audits a chain sees just its own receipts, and a server-wide `receipt_seq` disclosed how many
receipts other accounts had been issued between two of the owner's.

## Decision

Each owner's receipts form their own chain. A receipt names the owner whose chain it extends,
`receipt_seq` counts from 1 per owner, and `prior_receipt_hash` names that owner's previous
receipt. Position allocation and signing run under a lock on that owner's chain head inside the
finalization transaction, so one owner's chain cannot fork and finalizations for different owners
never wait for each other. The receipt format becomes `custody-receipt/v2`, which adds the owner
to the signed core; a `custody-receipt/v1` receipt from the server-wide chain still verifies
under the attestation key history and is kept as evidence.

## Consequences

- Finalization throughput is no longer bounded by one server-wide lock; contention is limited to
  one owner's concurrent finalizations.
- A receipt no longer reveals other accounts' upload volume.
- An owner can still prove a gap or a fork in its own chain, which is the audit a receipt holder
  can perform. Detecting a server that shows different chains to different auditors still needs
  gossiped signed heads, as before.
- Clients accept both receipt versions; a library's stored `v1` receipts are never rewritten.
- The server keeps one chain head per owner, and rebuild and the scrub verify chains per owner.
- The per-owner chain, the owner field and the per-owner lock are not implemented yet; the
  server-wide chain and its table lock remain until they are.
