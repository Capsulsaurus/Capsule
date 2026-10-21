# ADR-0007 — Revoking a capability revokes every successor refreshed from it

- **Status:** accepted
- **Date:** 2026-10-17
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Federation — Token Lifecycle](../capsule-docs/src/content/docs/design/federation.md#token-lifecycle-and-chain-of-trust)
- **Slices:** S-E2

## Context

A federation capability is refreshed rather than extended: `POST
/v1/federation/capabilities/refresh` presents the previous token, and the store links that
predecessor to the successor it mints and revokes the predecessor. The album owner, though,
only ever holds the `jti` they minted. `DELETE /v1/albums/{album_id}/capabilities/{jti}`
names that original `jti`, which after one refresh is already revoked.

Revoking only the named token would therefore answer `204` while the grant lived on in its
successor: the owner believes the peer is cut off, and the peer keeps pulling. The binary
(#479) resolved this without a record; its review found the choice unsanctioned, and the
maintainer sanctioned it as built (#522).

## Decision

Revoking an issued capability revokes the named token **and every successor refreshed from
it**. `revoke_issued` in
[`capsule-server/src/federation/postgres.rs`](../capsule-server/src/federation/postgres.rs)
walks the refresh chain and revokes and publishes every live link in one transaction, and
the conformance case
`revoking_a_refreshed_grant_by_its_original_jti_revokes_the_whole_chain` in
[`capsule-server/src/federation/conformance.rs`](../capsule-server/src/federation/conformance.rs)
holds every `CapabilityStore` adapter to it.

## Consequences

- The owner needs no view of the refresh chain to cut a grant: the `jti` they minted stays
  the handle for the grant's whole life.
- Every `CapabilityStore` adapter must keep the predecessor-to-successor link, because the
  conformance suite fails one that revokes only the named row.
- A revoke races refreshes of the same chain. The Postgres adapter locks only the chain's
  tail, the one link that can still be live, and starts over when a refresh commits between
  its read and its lock; a chain refreshed under every attempt is answered as the store
  being unavailable rather than as a revoke that did not reach the newest token.

## Considered and rejected

- **Revoke the named `jti` only.** Answers `204` for a grant that is still live once it has
  been refreshed, which is a silent failure of the one control an owner has.
