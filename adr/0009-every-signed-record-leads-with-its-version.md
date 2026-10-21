# ADR-0009 — Every signed record leads with its version, and unknown fields survive

- **Status:** accepted
- **Date:** 2026-10-21
- **Supersedes:** [ADR-0008](0008-the-sync-manifest-payload-is-an-unversioned-provenance-record.md)
- **Superseded by:** —
- **Contract:** [Versioning — System Invariants](../capsule-docs/src/content/docs/design/versioning.md#system-invariants)
- **Issues:** #665

## Context

ADR-0008 let the sync feed's `manifest_cbor` decode as a `ProvenanceRecord` with no version,
because no released peer spoke any other shape, and required the first post-release change to
add one. `ManifestCore` is a plain struct with no unknown-field map, so a manifest key an older
reader does not know is dropped on decode; the signature check then fails and the entry is
quarantined. That is safe but not forward-compatible: an older device can never apply a newer
manifest, even one that only adds an optional field. `SidecarV1` already does the right thing:
`sidecar_schema` is key 0 and unknown keys are kept, re-emitted canonically and signed over.

Nothing has been released, so this is the last point at which versioning the record costs no
migration.

## Decision

Every signed record — the provenance record and the manifest inside it, as well as the sidecar
— leads with a schema version decoded before anything else, and keeps unknown fields verbatim
under its signature. A reader that meets a newer version stores it and does not modify it; a
reader that meets unknown fields in a known version preserves them.

## Consequences

- `ProvenanceRecord` and `ManifestCore` change shape before the first release (#665); existing
  test libraries are regenerated rather than migrated.
- Golden fixtures per version become part of the test suite (#679), so a later reader is held
  to every earlier shape.
- ADR-0008's open obligation is discharged.
