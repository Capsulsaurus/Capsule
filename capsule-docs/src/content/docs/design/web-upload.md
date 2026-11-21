---
title: Web Upload
description: Browser-based guest drops — upload-link provisioning, the sealed drop format, and adoption into the library
audit: none
---

Web upload lets a guest with no Capsule account, the non-registered account class ([](/design/authentication/#account-types)) using the browser *web client*, send assets to a user. The guest holds no album key or MLS membership and cannot sign, so it never writes into the library:

1. the user **provisions an upload link**;
2. the guest's browser **encrypts each asset and seals its key to the link's Drop Key**;
3. the sealed bytes land in the user's **staging inbox**;
4. nothing becomes a library asset until the user **adopts it on a native client**, which authors the provenance.

This doc owns the upload-link capability, the drop wire object and the adoption transition. [](/design/cryptography/keys/#non-registered-accounts) owns the Drop Key and its escrow, [](/design/cryptography/encryption/#asset-key-derivation) the key rewrap, and [](/design/cryptography/provenance/#asset-manifest) `key_mode`/`wrapped_file_key`. Endpoints and error codes are in [](/reference/api/drops/), emitted by [`capsule-server/src/routes/drop.rs`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/routes/drop.rs).

## Two Confidentiality Properties

1. **Server-blind.** The server stores only ciphertext and a KEM-encapsulated key it cannot open. The Drop Key public half reaches the guest in the link's **URL fragment**, which browsers never transmit, so the server cannot substitute a key it controls.
2. **Contribute-only.** The guest holds only the public Drop Key half, with no AMK, device key or write-tier key. It cannot read the library, other guests' drops or the album's contents.

Server-blindness has the bound every browser surface has: the guest's browser runs code the server serves, and that code reads the fragment and sees the plaintext. Both properties hold against a server that serves honest code, as [](/design/share-links/#security-contract) states for share links, and an installed native client opens upload links itself.

## Scope

In v1: link provisioning with per-link caps (expiry, cumulative bytes, file count, per-file size, single- or multi-use) and revocation; browser drop upload of originals; a per-user inbox charged to the provisioning user's quota; adopt-in-place; discard.

Not in v1: guest-produced derivatives, metadata or provenance (the adopter produces them); guest read-back; adoption by anyone but the provisioning user (the Drop Key is escrowed only under their hierarchy); writable share links ([](/design/share-links/)).

## Security Contract

- **Link format.** `https://server.tld/u/{opaque_id}#{drop_pubkey}`. `{opaque_id}` follows the share-link opaque-id rule of [](/design/share-links/#security-contract) (≥128-bit CSPRNG, no structure), and `{drop_pubkey}` rides the fragment.
- **Drops never enter the library.** A drop lives only in the provisioning user's inbox, never on any sync feed ([](/design/sync/#discovering-what-changed)), and is served only to that user's devices. It carries no `AssetManifest` and never reaches `verify_asset` ([](/design/cryptography/keys/#write-authorization)). Library state is reachable only through adoption by a trusted client.
- **Per-link caps are enforced server-side at drop-session creation**, decided and reserved in one store operation so concurrent guests cannot both take the last slot. A leaked link is bounded to wasted quota and inbox space.
- **Quota is charged to the link owner at drop-session creation**, the single enforcement point ([](/design/quota/#enforcement-points)).
- **Probing reveals nothing.** An unknown, expired, revoked or spent link and an unknown session all return one indistinguishable `404`, never `410`. A cumulative cap on a live link is the distinct `409 error.drop.cap_exhausted`, because the guest holds a real link and needs a new one.
- **Rate limits.** Drop-session creation is limited per source IP, then per `{opaque_id}` ([](/design/authorization/#rate-limits); `429 error.drop.rate_limited`), before the link is resolved, failing closed. The source is resolved as [](/design/share-links/#security-contract) resolves it.
- **An optional passphrase gates the write, not confidentiality.** The link record stores an Argon2id verifier ([](/design/cryptography/primitives/#password-based-kdf)), and the guest's browser derives the same value from the passphrase. The guest never sends that value. It sends `passphrase_proof`, an HMAC-SHA-256 keyed by the derived value over a domain-separated message holding the opaque id, the guest's timestamp and a fresh 16-byte nonce, together with the timestamp and nonce. The server recomputes it from the verifier and compares in constant time **before any cap is reserved**, so wrong guesses cannot burn the link down. It refuses a timestamp more than **5 minutes** from its clock and a nonce this link saw in the last **10 minutes**, and spends the nonce only after the proof verifies. One captured request (a shared HAR file, a proxy log) is then useless after minutes, where a static proof would be a passphrase-equivalent for the link's lifetime. A refusal is `403 error.drop.passphrase_required`, not `404`, since the caller already holds the id. The verifier held by the server is itself passphrase-equivalent for the write gate, which is acceptable because the server already decides every write.
- **Home-server-only.** Like a share link, an upload link is served only by the album owner's home server ([](/design/federation/#album-ownership-v1-single-home-server)); a peer refuses with a `{ home_server }` pointer.
- **Adopted bytes are external-origin.** Every client decodes them only in the sandboxed decoder ([](/design/clients/#sandboxed-decoder)).
- **Before the sandbox ships.** The sandboxed decoder is post-v1 and web upload is in v1, so until a platform has the sandbox, that platform decodes drop bytes only through the memory-safe default codecs of `capsule-core::media` ([](/design/thumbnails/#media-is-rawshifts)), at review and at adoption, and never hands them to a platform image or video decoder. A drop those codecs cannot decode is reviewed from its descriptor alone (name, type, size) and adopted without derivatives; its derivatives are generated once the sandbox exists. This is the documented v1 deviation for this surface, not an exemption from the sandbox rule.

## Drop and Adoption Lifecycle

### 1. Provision (native client)

The client mints a Drop Key ([](/design/cryptography/keys/#non-registered-accounts)), escrows the private half under the master key and the user's OGK ([](/design/cryptography/keys/#owner-group-keys-ogks)), and registers the link record: opaque id, caps, pinned `protocol_version` and `crypto_suite_id`, and optional passphrase verifier. The server never sees `{drop_pubkey}`.

### 2. Seal and upload (web client)

Per asset, the browser draws a random 32-byte key `K`, encrypts under the unchanged STREAM construction ([](/design/cryptography/encryption/#stream-construction)) behind a blob header with `key_epoch = 0`, since `K` is not derived from an AMK ([](/design/cryptography/encryption/#blob-header)), and encapsulates `K` to `{drop_pubkey}` as `kem_ct`. It uploads the ciphertext with an unsigned `DropDescriptor`: content type, plaintext size, STREAM chunk size and nonce prefix, `ciphertext_hash`, `kem_ct` and an advisory `suggested_filename`. Transfer follows the chunk, finalization and session-lifetime rules of the upload protocol ([](/design/transfer/upload/), [](/design/transfer/upload/#session-lifetime-and-discard)) under link-capability auth.

A descriptor is not a manifest. It has no signatures, `album_id` or provenance, and is untrusted until a trusted client decapsulates `K` and the STREAM tags verify.

### 3. Stage (server)

The server validates the session against the link record and the drop invariants ([](/design/threat-model/validation/#on-post-dopaque_id-upload-link-drop-session-and-adoption)), charges quota, and stores the ciphertext as a content-addressed blob referenced by a drop-inbox row. The drop then appears on the user's native clients as a quarantine surface ([](/design/threat-model/scenarios/#quarantine-surfaces)) awaiting review.

- **Filed exactly once.** A finished drop session is deposited under the upload protocol's finalization claim ([](/design/transfer/upload/#finalization-and-integrity)). The server re-hashes the ciphertext against `ciphertext_hash` before committing it, then moves the held descriptor into the inbox in one store operation.
- **Detached.** A claimed deposit reaches a terminal state whatever happens to the guest's request or the server process. The guest reads its outcome through the drop's status probe under the link capability.
- **Recovered.** A full drop session nobody claimed, because the process died between recording its last chunk and claiming, is deposited on the next boot, or sooner when the guest resends one of its chunks.
- **Reservation returned.** A deposit that fails before filing, or a drop session discarded under pressure or for a missing stage, gives the link its reservation back.

### 4. Review and adopt-in-place (native client, provisioning user)

The client decapsulates `K`, previews in the sandboxed decoder, and on approval:

1. **Checks the guest's values before signing any of them.** The descriptor is guest-asserted, and the adopter signs `ciphertext_hash` and `nonce_prefix` it did not produce. In one streaming pass, with bounded memory, it decrypts every chunk (every STREAM tag verifies), re-encrypts the plaintext under `K`, the descriptor's nonce prefix and `key_epoch = 0`, and requires the result to be byte-identical to the staged ciphertext, its plaintext length to equal `plaintext_size` and its chunk size to equal the suite's ([](/design/cryptography/encryption/#stream-construction)). Any mismatch refuses the drop. A guest therefore cannot choose a chunk size, a size claim or a non-canonical encoding that would make the asset fail its own later verification.
2. Authors the signed sidecar ([](/design/metadata/#sidecar-schema-v1)) with an unverified guest-origin note. The guest is never a signer.
3. Wraps `K` under the destination album's AMK (`asset-keywrap/v1`) as `wrapped_file_key`. An externally chosen `K` cannot be derived, so it is carried.
4. Signs a `create` manifest with `ciphertext_hash` and `nonce_prefix` from the drop, `key_mode = wrapped` and the **adopter** as author, and appends the `create` provenance record.
5. Submits it to `POST /v1/drops/{drop_id}/adopt`. The server re-runs the create validation battery and requires the referenced blob to be a drop in the **caller's own inbox**. Only the metadata and provenance blobs are new bytes. The ciphertext hash is not recomputed, since that ran at the drop's finalization.

Server-side adoption is **claim → write asset → settle**. Writing first could duplicate a photo and deleting first could lose one, so a crash leaves the row visible in the owner's inbox marked `adopting`, never lost or duplicated. The client drops its local inbox row only after the asset is written locally. Afterwards the asset is ordinary: later edits use the derived-key path, and only this `create` sets `key_mode = wrapped`.

### 5. Discard

A rejected or expired drop's inbox row is removed and its blob garbage-collected ([](/design/filesystem/server/#deletion-and-garbage-collection)), freeing quota. No provenance is involved.

## Why Adopt-in-Place

Re-encrypting under a derived key would upload and store every asset twice. Rewrapping the 32-byte key keeps one copy, at the cost of one contained divergence: the adopted asset's file key is carried wrapped rather than derived. It affects only key recovery, never authorization, provenance or STREAM.

## Failure Modes

- **Leaked link:** bounded by caps, quota, rate limits and revocation; it cannot inject into the library.
- **Server reads drops:** impossible without `{drop_pubkey}`, which it never receives, while it serves honest code ([](#two-confidentiality-properties)).
- **Hostile file:** decoded only in the sandbox, after owner review; until the sandbox ships, only by the memory-safe default codecs ([](#security-contract)).
- **Captured passphrase proof:** fresh for minutes and single-use.
- **Guest-chosen STREAM parameters:** refused unless the drop is the canonical encryption of its plaintext.
- **Forged `wrapped_file_key` or mismatched `key_mode`:** covered by `device_sig` and `write_sig`; fails `verify_asset` and is quarantined.
- **Weak guest `K`:** compromises only that asset.

## Known Gaps

- `passphrase_proof` is the static Argon2id output. The browser also sends it as hex while the server decodes base64, so a passphrase-gated link refuses every honest guest, and no native client provisions a verifier.
- Adoption decrypts and re-encrypts the whole drop in memory.
- `DropAdopter::adopt` has no caller outside tests.

## Validation

- Seal → decapsulate → STREAM-decrypt round-trips byte-identically; `kem_ct` length matches the suite.
- Upload-link ids are ≥128-bit and non-sequential.
- An adopted manifest with `key_mode = wrapped` passes `verify_asset`, and a second member unwraps and decrypts the unchanged ciphertext; a forged wrapped key or mismatched `key_mode` is rejected.
- A `DropDescriptor` cannot reach any album-write path, and the inbox never appears on the sync feed.
- Caps, quota, the per-source and per-link limiters, and the indistinguishable `404` each refuse as specified.
- A crash mid-adoption leaves an `adopting` row, never a lost or duplicated asset.
- A replayed, stale or wrong `passphrase_proof` is refused before any cap is reserved, and a proof produced by the web client verifies on the server (a cross-implementation vector pins the encoding).
- A drop with a wrong chunk size, a false `plaintext_size` or a non-canonical encoding is refused before anything is signed, within the adoption memory bound.
