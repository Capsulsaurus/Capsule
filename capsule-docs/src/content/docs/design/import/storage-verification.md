---
title: Storage Verification
description: The endpoint a client calls to confirm an asset is durably stored, indexed, and retrievable before any destructive local action
---

[Upload finalization](/design/import/upload-protocol/#finalization-and-integrity) confirms, **once**, that the bytes the server assembled match the declared hash. That `Completed` acknowledgement is a transfer receipt, not a standing durability guarantee a client can re-check later — and [`verify_asset`](/design/cryptography/keys/#write-authorization) proves *cryptographic* validity and authorization, never that the server still holds the bytes. A client that is about to discard local data therefore has no way, today, to ask the server: *do you actually still have this, indexed and retrievable?*

This doc defines that missing query and the rule that every client follows before any destructive local action. The endpoint lives in `capsule-api-media` (it answers from the blob store plus the Postgres index); the client-side gate is a pure predicate in `capsule-core` invoked from `capsule-sdk`.

## What "Safely Stored" Means

A blob being content-addressed and hash-matching is necessary but not sufficient — a hash match only proves the *bytes the client holds* are internally consistent, not that the *server* has them in a serveable state. "Safely stored" is the conjunction of three independent facts the server can attest **without any key**:

- **Stored** — the ciphertext blob is present in the [blob store](/design/filesystem/server/#blob-store-layout) at its content address (`stat` of `blobs/{hash[0:2]}/{hash[2:4]}/{hash}`), not merely an in-flight `incoming/` chunk.
- **Indexed** — a committed Postgres row references the blob with `uploaded = true`, the asset's [provenance chain head](/design/cryptography/provenance/#provenance-of-library-modifications) is current, and the blob's role on the asset is recorded (see [PostgreSQL: What the Server Knows](/design/filesystem/server/#postgresql-what-the-server-knows)).
- **Retrievable** — the blob is in a state the server would actually serve: reference count > 0, **not** marked `collectable_since` (mid-[GC](/design/filesystem/server/#deletion-and-garbage-collection)), **not** quarantined, and not a dangling-reference integrity error.

A `durable` verdict requires all three to hold for every **required** blob of the asset — its original and metadata blobs, plus any derivative the client declares it is relying on.

## Endpoint

| Method | Path              | Purpose                                                                                       |
| ------ | ----------------- | --------------------------------------------------------------------------------------------- |
| `POST` | `/storage/verify` | Batch-confirm that the listed assets' blobs are stored, indexed, and retrievable on the server |

Authenticated with the same bearer JWT and run through the universal [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation) as every other surface. The request is a read; it writes no state.

The client declares the **exact** blob hashes it is relying on, so the verdict confirms *those* bytes rather than "some version of this asset" — a server that silently holds a different ciphertext cannot answer `durable` for a hash it does not have:

```jsonc
// POST /storage/verify
{
  "assets": [
    { "asset_id": "…", "blob_hashes": ["<original>", "<metadata>", "<thumb>"] }
  ],
  "deep": false   // optional; see below
}
```

The response is one verdict per asset:

```rust
StorageVerification {
  asset_id:   UUID,
  durable:    bool,                 // all required blobs stored && indexed && retrievable
  blobs: [ {
    hash:        bytes,
    role:        enum,              // original | metadata | derivative | provenance
    stored:      bool,             // present in blobs/ (stat)
    indexed:     bool,             // referenced by a committed, uploaded=true row
    retrievable: bool,             // refcount > 0, not collectable_since, not quarantined
  } ],
  checked_at: RFC3339,              // server's trusted clock, like received_at
}
```

- A hash the client lists that the server does not associate with the asset comes back `stored=false, indexed=false` — surfaced, never silently omitted.
- **`deep`** (default `false`) asks the server to re-read and re-hash the blob bytes rather than trusting the `stat` + index, catching silent bit-rot at the cost of I/O. It is opt-in because the structural check answers the common case (did finalization actually durably land?) cheaply, while a deep scan is for periodic integrity audits.

The endpoint is deliberately cheap and idempotent so clients can call it freely on the destructive path below.

## Verify Before Destroy

The standing rule, enforced across every client: **after any write the server is expected to durably persist, a client confirms `durable = true` from `/storage/verify` (and that [`verify_asset`](/design/cryptography/keys/#write-authorization) accepts the asset) before any post-write local cleanup of data tied to that write.** The two checks are complementary and both required — `verify_asset` for cryptographic validity, `/storage/verify` for server durability — before irreplaceable local bytes are dropped.

Concretely, the gate applies to:

- **Releasing or evicting a device-owned original.** An original the device itself uploaded is the source of truth until the server durably holds it; it is [exempt from automatic eviction](/design/filesystem/client/#space-recovery) and may be released only after a `durable` verdict, after which it becomes an ordinary server-only, re-fetchable asset.
- **Move-mode import source deletion.** Deleting the external source after an import (Move mode) waits on `durable` — never on the local library copy alone — so a crash mid-import cannot lose the only copy. This is load-bearing for [streaming import](/design/import/pipeline/), where the local copy is also released.
- **Streaming-mode release.** The sliding-window import→upload→**verify**→release loop releases each asset only on its `durable` verdict.
- **Discarding any local working state tied to a write** — staged temporaries, the pre-edit copy retained across a `replace`/`metadata-update`, and similar — once the new state is confirmed durable.

The gate **does not** apply to:

- **Intentional deletes** — trash/soft-delete and hard-purge are the *purpose* of the operation, not post-write cleanup; their safety is the [retention window](/design/organization/#retention-window) and provenance tombstone, not a durability check.
- **Rebuildable cache** — thumbnails, previews, transcodes, and the SQLite index regenerate locally, so their eviction is always safe (see [Space Recovery](/design/filesystem/client/#space-recovery)).
- **Re-fetchable server-origin blobs** — a fetched-but-unpinned original came *from* the server, so the server is already known to hold it; evicting it is safe because it transparently re-fetches.

A non-`durable` verdict never triggers a destructive action: the client retains the local copy, retries verification with backoff, and surfaces the asset as "not yet confirmed on server" rather than silently dropping it.

## Relationship to Other Checks

- **vs. upload finalization.** Finalization's `Completed` is a one-time receipt for one transfer; `/storage/verify` is the re-checkable, after-the-fact confirmation that the receipt still holds — across app restarts, across devices, and after server-side GC or migration could have changed state.
- **vs. `verify_asset`.** `verify_asset` is local and key-aware (signatures, provenance chain, write authorization); `/storage/verify` is remote and key-free (does the server physically have and serve these bytes). Neither subsumes the other.
- **vs. the sync feed.** `/sync` tells a client *what exists*; `/storage/verify` tells it *whether a specific blob is durably serveable right now* — the question that gates destruction.

## Validation

- **Durable verdict (smoke).** Upload an asset to completion; `POST /storage/verify` with its blob hashes; assert `durable = true` and every blob `stored && indexed && retrievable`.
- **Partial / missing verdict (unit).** Verify an asset whose metadata blob never finalized; assert `durable = false` with the metadata blob `indexed = false`, and the original still reported accurately.
- **Mid-GC / quarantined blob (unit).** Mark a referenced blob `collectable_since` (or quarantine it); assert it reports `retrievable = false` and the asset is `durable = false`.
- **Wrong-hash declaration (unit).** Declare a blob hash the server does not hold for the asset; assert `stored = false, indexed = false` rather than a silent omission.
- **Verify-before-destroy gate (smoke).** Drive a device-owned-original release with the endpoint mocked to return non-`durable`; assert the client refuses to evict and surfaces the unconfirmed state; flip to `durable`; assert the release proceeds.
- **Deep scan (unit).** Corrupt a stored blob's bytes on disk; assert the structural check still reports `stored=true` but `deep = true` reports a hash mismatch.

The cross-module case — upload → finalize → `/storage/verify` durable → safe local release — is exercised inside the [full lifecycle](/design/module-map/#e2e-test-surface) E2E surface rather than as a standalone case.
