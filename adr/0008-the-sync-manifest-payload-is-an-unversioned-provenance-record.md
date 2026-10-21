# ADR-0008 — The sync manifest payload is an unversioned provenance record

- **Status:** accepted
- **Date:** 2026-10-17
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Download & Sync — Resumption and Verification](../capsule-docs/src/content/docs/design/transfer/download.md#resumption-and-verification)
- **Slices:** S-P5

## Context

Each sync feed entry carries `manifest_cbor`: the exact bytes of the asset's provenance
blob, which are the canonical CBOR of a `ProvenanceRecord` rather than of a bare
`AssetManifest`. A receiver has to decode them before it can verify or apply anything, so
the shape of those bytes is a wire contract between the server that serves them and every
device that applies them.

The binary (#479) decoded them with no envelope version and no negotiation, and recorded
why only in a code comment. Its review found the choice unsanctioned, and the maintainer
sanctioned it as built (#522).

## Decision

`manifest_cbor` is decoded as a `ProvenanceRecord` directly, with no version field and
nothing negotiated with the sending server, because **no released peer** speaks any other
shape: every server and client that speaks this feed is built from this tree. Bytes that
are not a record are quarantined as `MalformedManifest`, never guessed at. The decode and
its rationale are in
[`capsule-core/src/lifecycle/sync_apply.rs`](../capsule-core/src/lifecycle/sync_apply.rs),
module docs section "Why the payload carries no version".

## Consequences

- The first change to the record's shape after a release must introduce the version rather
  than assume one; a reader that finds a released peer has made this decision stale.
- The sealed sidecar inside an entry is unaffected: it carries its own `sidecar_schema`,
  which the receiver refuses forward.
- Until then there is no compatibility path to maintain and none to test.
