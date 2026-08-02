---
title: Storage Verification
description: The endpoint a client calls to confirm an asset is durably stored, indexed, and retrievable before any destructive local action
status: draft
---

[Upload finalization](/design/import/upload-protocol/#finalization-and-integrity) confirms, **once**, that the bytes the server stored match the declared hash. That `Completed` acknowledgement is a transfer receipt, not a standing durability guarantee a client can re-check later — and [`verify_asset`](/design/cryptography/keys/#write-authorization) proves *cryptographic* validity and authorization, never that the server still holds the bytes. A client that is about to discard local data therefore has no way, today, to ask the server: *do you actually still have this, indexed and retrievable?*

This doc defines that missing query and the rule that every client follows before any destructive local action. The endpoint is planned in `capsule-api::blob` (it answers from the blob store plus the Postgres index); the client-side gate is a pure predicate in `capsule-core` invoked from the planned `capsule-sdk`, complementing the offline core's cryptographic `verify_asset` gate. It is also the **single source of truth** for the two server-signed objects that make custody and loss *provable* rather than merely checkable: the [`CustodyReceipt`](#custody-receipts) and the [signed storage attestation](#signed-storage-attestation), composed in [Proof of Loss](#proof-of-loss).

## What "Safely Stored" Means

A blob being content-addressed and hash-matching is necessary but not sufficient — a hash match only proves the *bytes the client holds* are internally consistent, not that the *server* has them in a serveable state. "Safely stored" is the conjunction of three independent facts the server can attest **without any key**:

- **Stored** — the ciphertext blob is present in the [blob store](/design/filesystem/server/#blob-store-layout) at its content address (`stat` of `blobs/{hash[0:2]}/{hash[2:4]}/{hash}`), not merely an in-flight `incoming/` chunk.
- **Indexed** — a committed Postgres row references the blob with `uploaded = true`, the asset's [provenance chain head](/design/cryptography/provenance/#provenance-of-library-modifications) is current, and the blob's role on the asset is recorded (see [PostgreSQL: What the Server Knows](/design/filesystem/server/#postgresql-what-the-server-knows)).
- **Retrievable** — the blob is in a state the server would actually serve: reference count > 0, **not** marked `collectable_since` (mid-[GC](/design/filesystem/server/#deletion-and-garbage-collection)), **not** quarantined, and not a dangling-reference integrity error.

A `durable` verdict requires all three to hold for every **required** blob of the asset — its original and metadata blobs, plus any derivative the client declares it is relying on.

`durable` attests **this home server's** storage only. It says nothing about replicas, peers, or off-server backups — multi-location redundancy is the [backup artifact](/design/backup-recovery/)'s job — and a deployment running its own replication layer still attests only what this server can itself `stat` and serve.

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
- **`deep`** (default `false`) asks the server to re-read and re-hash the blob bytes rather than trusting the `stat` + index, catching silent bit-rot at the cost of I/O. It is opt-in because the structural check answers the common case (did finalization actually durably land?) cheaply, while a deep scan is for periodic integrity audits. `deep` is a server-priced operation: rate-limited per user and coalesced server-side (a repeated `deep` request against the same blob within a window returns the cached result), so a client cannot turn it into an I/O-amplification attack.

The endpoint is deliberately cheap and idempotent so clients can call it freely on the destructive path below.

## Custody Receipts

Everything above answers *is it still there?* — a cooperative, unsigned, point-in-time fact. It cannot answer the accountability question: *can either side later prove what the server was entrusted with?* Without that, "the server lost my photo" and "the client never uploaded it" are symmetric, unfalsifiable claims. The custody receipt closes both directions.

```rust
CustodyReceipt {
  version:            "custody-receipt/v1",
  crypto_suite_id:    u16,
  protocol_version:   String,           // YYYY-MM-DD
  server_id:          String,           // this server's canonical origin — binds the receipt to one server
  server_key_id:      bytes,            // fingerprint of the attestation key that signed; survives rotation
  receipt_seq:        u64,              // strictly monotonic per server
  prior_receipt_hash: Option<[u8;32]>,  // SHA-256 over the previous receipt in the server's log; null only
                                        //   for the first receipt — the provenance chain's append-only
                                        //   discipline, applied to the server's own log
  upload_id:          UUID,             // the session that produced custody
  asset_id:           UUID,
  blob_role:          enum,             // original | derivative | metadata | provenance
  ciphertext_hash:    bytes,            // RECOMPUTED by the server at finalization — never echoed from the client
  size:               u64,
  envelope_hash:      Option<[u8;32]>,  // SHA-256 of the manifest envelope CBOR — binds the receipt to the
                                        //   asset's provenance-chain position
  uploaded_by_user:   UUID,
  uploaded_by_device: Option<UUID>,
  received_at:        RFC3339,          // server's trusted clock at the finalization commit

  server_sig:         Hybrid(Ed25519, ML-DSA-65),  // under the server attestation key, over all fields above
}
```

Canonical CBOR with the same [wire-presence discipline](/design/cryptography/provenance/#asset-manifest) as manifests (absent optionals encode as absent keys), and deliberately server-visible — it carries ciphertext hashes only, no plaintext. Receipts are the server-signed complement of the client-signed [provenance chain](/design/cryptography/provenance/#provenance-of-library-modifications): the envelope proves what a client *claimed and signed*; the receipt proves what the server *accepted*, over a hash it recomputed itself.

- **Issued inside the finalization transaction** ([Upload Protocol — Finalization](/design/import/upload-protocol/#finalization-and-integrity)): the receipt insert commits atomically with the `uploaded` flip. No receipt without durable custody; no custody-marking without a receipt.
- **Signed under the server attestation key** — a long-lived **hybrid Ed25519 + ML-DSA-65** keypair, distinct from the classical [operational key](/design/federation/#server-identity-and-key-rotation). A receipt is evidence for the life of the asset, so it sits outside the [operational-signature carve-out](/design/cryptography/primitives/#signature-scheme). The key is published, pinned, and rotated with the server identity, with an **append-only key history** so old receipts verify forever; `server_key_id` selects the verification key.
- **Hash-chained and enumerable.** `receipt_seq` + `prior_receipt_hash` form the server's append-only receipt log, persisted like envelopes — Postgres rows plus content-addressed objects in the blob store — with **no API path that overwrites or deletes** an entry. The chain lets the server affirmatively enumerate everything it has ever accepted, and bounds post-key-compromise forgery: a "receipt" that does not chain into the log is provably out-of-band.
- **Fetched** via `GET /upload/{id}/receipt` in the session window (pairing with the [lost-ACK recovery flow](/design/import/upload-protocol/#idempotency-and-resumption)) and `GET /assets/{asset_id}/receipts` durably (uploader or owner). Receipts are permanent objects, exempt from session GC.
- **Client persistence and verification.** The client appends receipts beside the provenance chain (`media/{YYYY}/{YYYY-MM}/{uuid}.receipts.cbor`) and includes them in the [backup artifact](/design/backup-recovery/). Unlike the provenance cache, this copy is **evidentiary, not merely a cache** — a server destroying the record of its own liability is exactly the adversary — so the client copy is first-class. On fetch the client verifies the signature under the pinned attestation key, that `ciphertext_hash`/`size`/`envelope_hash` match what it sent, and that `received_at` passes a gross-drift sanity bound (evidentiary only; ordering rides the chain, mirroring the manifest [`timestamp` rule](/design/cryptography/keys/#write-authorization)).

## Signed Storage Attestation

The `/storage/verify` verdict gains an on-request signed form: the request accepts `signed: true` and an optional client `nonce`.

```rust
StorageAttestation {
  version:         "storage-attestation/v1",
  crypto_suite_id: u16,
  server_id:       String,
  server_key_id:   bytes,
  nonce:           Option<bytes>,       // client-supplied freshness challenge, echoed verbatim —
                                        //   a stale durable=true cannot be replayed as current
  verdict:         StorageVerification, // the unchanged verdict above, incl. checked_at
  server_sig:      Hybrid(Ed25519, ML-DSA-65),  // attestation key, over all fields above
}
```

Signing is server-priced exactly like `deep` — rate-limited per user and coalesced — so the hot verify-before-destroy loop stays on the unsigned verdict. The signed form exists for *evidence*: an honest server self-reporting loss (`stored = false` under its own signature) and periodic signed audits a client retains.

## Proof of Loss

The two objects compose into transferable proof, with the burden of proof placed where it can actually be discharged.

**A client proves the server lost entrusted bytes** with three elements:

1. **Acceptance** — a `CustodyReceipt` for hash `H`, verifying under the server's published attestation key: the server took custody at `received_at`.
2. **Later non-holding** — either a signed `StorageAttestation` reporting `H` non-`stored`/non-`durable` (the cooperative path), or a **failed retrieval challenge**: anyone can fetch `H` by content address, and content addressing makes the response self-verifying, so a 404 or wrong bytes is independently checkable evidence even against an uncooperative server.
3. **No authorized purge** — the server's legitimate rebuttal is the asset's own provenance chain it already holds: a device-signed `delete` manifest with an elapsed [`retention_until`](/design/organization/#retention-window) proves it was *dis*entrusted, not negligent. A verifier checks any presented delete against the receipt's `envelope_hash` chain position.

**The server proves what it was — and was not — entrusted with** by a burden shift: a custody claim is valid only when backed by a receipt under the server's signature, so *"no valid receipt exists"* is the negative proof, resting on signature unforgeability rather than log completeness. The chained receipt log additionally lets the server enumerate its accepted set for audit; what clients *claimed* is already the device-signed envelope chain it persists.

Residual limitation, accepted for v1: a dishonest server can show different receipt-log views to different auditors (split view). Any client holding a receipt at `receipt_seq = N` proves the log has at least `N` entries, which bounds silent truncation; gossiped signed log heads over the sync feed are the designated v2 extension — the receipt-log counterpart of the provenance chain's [v2 checkpoint note](/design/cryptography/provenance/#chained-append-only-structure).

A server that simply **refuses to issue receipts** gains nothing: the release gate below refuses to ever make such a server the sole holder of an only-copy.

## Verify Before Destroy

The standing rule, enforced across every client: **after any write the server is expected to durably persist, a client (1) holds and has verified the write's [`CustodyReceipt`](#custody-receipts), (2) confirms `durable = true` from `/storage/verify`, and (3) confirms [`verify_asset`](/design/cryptography/keys/#write-authorization) accepts the asset — before any post-write local cleanup of data tied to that write.** The three checks are complementary and all required before irreplaceable local bytes are dropped: the receipt for accountability (a server that withholds receipts never becomes the sole holder of an only-copy), `/storage/verify` for durability, `verify_asset` for cryptographic validity.

Concretely, the gate applies to:

- **Releasing or evicting a device-owned original.** An original the device itself uploaded is the source of truth until the server durably holds it; it is [exempt from automatic eviction](/design/filesystem/client/#space-recovery) and may be released only after a `durable` verdict, after which it becomes an ordinary server-only, re-fetchable asset.
- **Move-mode import source deletion.** Deleting the external source after an import (Move mode) waits on `durable` — never on the local library copy alone — so a crash mid-import cannot lose the only copy. This is load-bearing for [streaming import](/design/import/pipeline/#import-upload-streaming-mode), where the local copy is also released.
- **Streaming-mode release.** The sliding-window import→upload→**verify**→release loop releases each asset only on its `durable` verdict.
- **Discarding any local working state tied to a write** — staged temporaries, the pre-edit copy retained across a `replace`/`metadata-update`, and similar — once the new state is confirmed durable.

The gate **does not** apply to:

- **Intentional deletes** — trash/soft-delete and hard-purge are the *purpose* of the operation, not post-write cleanup; their safety is the [retention window](/design/organization/#retention-window) and provenance tombstone, not a durability check.
- **Rebuildable cache** — thumbnails, previews, transcodes, and the SQLite index regenerate locally, so their eviction is always safe (see [Space Recovery](/design/filesystem/client/#space-recovery)).
- **Re-fetchable server-origin blobs** — a fetched-but-unpinned original came *from* the server, so the server is already known to hold it; evicting it is safe because it transparently re-fetches.

A non-`durable` verdict never triggers a destructive action: the client retains the local copy, retries verification with backoff, and surfaces the asset as "not yet confirmed on server" rather than silently dropping it.

**The verdict is a point-in-time fact**, so the verify→release window is kept tight on both sides: the client re-verifies if more than a bounded interval (default 60 s) elapses between verdict and release, and on the server a blob that just answered `durable` cannot reach byte deletion faster than the standing [GC grace window](/design/filesystem/server/#deletion-and-garbage-collection) — the verdict plus that grace period is what makes the gate sound without a lease protocol.

## Relationship to Other Checks

- **vs. upload finalization.** Finalization's `Completed` is a one-time receipt for one transfer; `/storage/verify` is the re-checkable, after-the-fact confirmation that the receipt still holds — across app restarts, across devices, and after server-side GC or migration could have changed state.
- **vs. `verify_asset`.** `verify_asset` is local and key-aware (signatures, provenance chain, write authorization); `/storage/verify` is remote and key-free (does the server physically have and serve these bytes). Neither subsumes the other.
- **vs. the sync feed.** The sync feed tells a client *what exists*; `/storage/verify` tells it *whether a specific blob is durably serveable right now* — the question that gates destruction.
- **vs. custody receipts.** The verdict says whether the bytes are serveable *now*; the [receipt](#custody-receipts) proves the server accepted them *then*. Detection needs the verdict; accountability needs both (see [Proof of Loss](#proof-of-loss)).

## Validation

- **Durable verdict (smoke).** Upload an asset to completion; `POST /storage/verify` with its blob hashes; assert `durable = true` and every blob `stored && indexed && retrievable`.
- **Partial / missing verdict (unit).** Verify an asset whose metadata blob never finalized; assert `durable = false` with the metadata blob `indexed = false`, and the original still reported accurately.
- **Mid-GC / quarantined blob (unit).** Mark a referenced blob `collectable_since` (or quarantine it); assert it reports `retrievable = false` and the asset is `durable = false`.
- **Wrong-hash declaration (unit).** Declare a blob hash the server does not hold for the asset; assert `stored = false, indexed = false` rather than a silent omission.
- **Verify-before-destroy gate (smoke).** Drive a device-owned-original release with the endpoint mocked to return non-`durable`; assert the client refuses to evict and surfaces the unconfirmed state; flip to `durable`; assert the release proceeds.
- **Deep scan (unit).** Corrupt a stored blob's bytes on disk; assert the structural check still reports `stored=true` but `deep = true` reports a hash mismatch.
- **Receipt issuance (smoke).** Upload to `Completed`; fetch the receipt; verify the hybrid signature against the published attestation key; assert `ciphertext_hash`, `size`, `blob_role`, `envelope_hash`, and `received_at` match the finalized state.
- **No receipt without custody (unit).** Inject a failure between hash verification and the finalization commit; assert no receipt exists and `receipt_seq` did not advance — receipt and `uploaded` flip are atomic.
- **Receipt log append-only + monotonic (unit).** Finalize two blobs; assert strictly increasing `receipt_seq` and correct `prior_receipt_hash` chaining; assert any overwrite or delete attempt on a receipt is rejected at the structural layer (invariant 33).
- **Signed attestation (unit).** Request `signed: true` with a nonce; verify the signature and the nonce echo; mutate any verdict field or the nonce; assert verification fails (invariant 34).
- **Proof-of-loss composition (smoke).** Finalize and fetch the receipt; delete the blob bytes server-side; a signed verify reports `stored = false`; assert the (receipt, attestation) pair verifies as a loss proof under the same `server_key_id`, and that a content-addressed fetch of the hash fails.
- **Delete rebuttal (unit).** Finalize, then run a signed `delete` plus retention purge; assert the server can present receipt + delete envelope such that a verifier classifies the non-holding as authorized purge, not loss.
- **Cross-server replay rejection (unit).** Present server A's receipt as evidence against server B; the verifier rejects on the `server_id`/`server_key_id` binding.
- **Receipt-gated release (smoke).** Drive a device-owned-original release with the receipt fetch failing; assert the client refuses to release even on `durable = true`; restore the endpoint; the release proceeds.
- **Rotation continuity (unit).** Rotate the attestation key; assert a pre-rotation receipt still verifies via `server_key_id` against the published key history.

The cross-module case — upload → finalize → `/storage/verify` durable → safe local release — is exercised inside the [full lifecycle](/design/module-map/#e2e-test-surface) E2E surface rather than as a standalone case.
