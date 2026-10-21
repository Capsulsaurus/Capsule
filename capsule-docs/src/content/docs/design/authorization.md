---
title: Authorization
description: The closed lifecycle-action set and how every destructive operation is signed and audited
status: draft
---

**Rule:** every lifecycle transition is an [asset manifest](/design/cryptography/provenance/#asset-manifest) signed under the album's per-epoch write-tier key — the same proof as a write. There is no weaker path to destroy data than to add it.

Clients reject anything [`verify_asset`](/design/cryptography/keys/#write-authorization) does not accept.

## The Closed Action Set

`action` is a **closed enum**; any other value is a structural error ([Schema Rules](/design/threat-model/schema-rules/#closed-enums)).

| Action | Meaning |
| --- | --- |
| `create` | First write; `prior_provenance_hash` is `null` |
| `replace` | New original bytes (e.g. re-encryption), same `file_id`/`album_id` |
| `delete` | Soft-delete into trash for a [retention window](/design/organization/#retention-window) |
| `metadata-update` | Edit the encrypted metadata blob or sidecar fields |
| `derivative-add` | Add a [derivative](/design/cryptography/provenance/#derivative-provenance) |
| `derivative-replace` | Replace a derivative — the only path; silent overwrite is rejected |
| `trash-restore` | Recover from trash within the window |

Adding a value requires a new `protocol_version`; albums stay pinned to their original set.

## Authorizing a Lifecycle Operation

A client accepts an operation only if:

- it is signed with the album's write-tier key for its epoch, **and** carries a valid hybrid `device_sig`, both checked in `verify_asset`;
- its `prior_provenance_hash` equals the current chain head (every action but `create`); stale or forked positions are rejected.

## The Lifecycle Write Surface

Every non-upload lifecycle write goes through **one** endpoint, `POST /v1/albums/{album_id}/ops`, carrying the signed manifest (plus metadata blob if any). Writes that move blob bytes (`create`, `replace`, derivatives with new bytes) use the [upload protocol](/design/transfer/upload/).

- The structural envelope ([server invariants](/design/threat-model/validation/#server-side-validation-invariants) 16–18, 25) runs before any row is written. New actions never add endpoints.
- **Idempotent by content hash:** a replayed accepted manifest returns the identical response and writes nothing.
- Provenance append and per-album `sync_seq` mint are **one transaction** — visible on the [sync feed](/design/transfer/download/) exactly when durable.
- Rejections carry an [`error.*` code](/design/i18n/#server-error-codes), write nothing, and are logged.

## Album Membership on the Server

The server cannot read the MLS roster. It holds the owner's **signed roster** (`PUT /v1/albums/{album_id}/roster`, [invariant 33](/design/threat-model/validation/#server-side-validation-invariants)): members with role `reader`/`writer`, a strictly increasing `roster_version` (at most +16 over the held one, so one publish cannot latch the counter), and the AMK epoch, signed by a non-revoked owner device. Only the owner publishes; removal is a later roster omitting the member, recorded with the version and epoch, not deleted.

| Caller | Server behaviour |
| --- | --- |
| Current writer | May upload and post ops; filed under the owner's namespace, billed to the uploader |
| Current member (any role) | May fetch blobs |
| Former member | `403` ([Transfer — Download](/design/transfer/download/)) |
| Never on roster | `404`, as for an unknown address |

The roster is a transport control, not confidentiality. Removing a writer reclaims nothing: their uploads stay, still billed to them, deletable only by the owner or a current writer ([Quota — Scope Decisions](/design/quota/#scope-decisions)).

## The Server Executes But Never Authorizes

The server carries out deletes and replaces but never permits them. Without keys it still enforces the [structural envelope](/design/threat-model/validation/#server-side-validation-invariants) before persisting.

## Deletes Are Soft First

- `delete` moves the asset to trash; the blob is hard-purged only after the retention window.
- The window is **signed into the delete manifest**, so a server cannot shorten or extend it ([Recycling](/design/organization/#recycling)).
- `trash-restore` before expiry is itself an audited manifest.

## Every Transition Is Auditable

Each operation appends a [provenance record](/design/cryptography/provenance/#provenance-of-library-modifications); the chain is [append-only](/design/threat-model/scenarios/#provenance-immutability-rules).

## Federated Peers

[Federated](/design/federation/) operations pass the same `verify_asset` and envelope checks; peer ordering and timestamps are never trusted. [Stale revival](/design/sync/#stale-revival-detection) is caught by the chain check and quarantined.

## Validation

- Per action: correct keys verify; wrong write-tier key, device, or `prior_provenance_hash` is rejected; unknown `action` fails the envelope; a stale chain position is quarantined.
- Invariants 16–18 against real Postgres; full lifecycle E2E in [Module Map](/design/module-map/#e2e-test-surface).
