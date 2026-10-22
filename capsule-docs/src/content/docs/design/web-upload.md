---
title: Web Upload
description: Browser-based guest drops — upload-link provisioning, the sealed drop format, and adoption into the library
status: draft
---

Web upload lets a guest with no Capsule account — the [non-registered account](/design/authentication/#account-types) class, using the browser *web client* — send assets to a user. The guest holds no album key and no MLS membership and cannot sign, so it never writes into the library: the user **provisions an upload link**, the guest's browser **encrypts each asset and seals its key to the link's Drop Key**, the sealed bytes land in the user's **staging inbox**, and nothing becomes a library asset until the user **adopts it on a native client**, which authors the provenance.

This doc owns the upload-link capability, the drop wire object, and the adoption transition. The Drop Key and its escrow are owned by [Keys — Non-registered accounts](/design/cryptography/keys/#non-registered-accounts); the key rewrap by [Encryption — Asset Key Derivation](/design/cryptography/encryption/#asset-key-derivation); `key_mode`/`wrapped_file_key` by [Provenance — Asset Manifest](/design/cryptography/provenance/#asset-manifest). Endpoints and error codes are defined by [`capsule-server/src/routes/drop.rs`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/routes/drop.rs) and the served OpenAPI contract.

## Two Confidentiality Properties

1. **Server-blind.** The server stores only ciphertext and a KEM-encapsulated key it cannot open. The Drop Key public half reaches the guest in the link's **URL fragment**, which browsers never transmit, so the server cannot substitute a key it controls.
2. **Contribute-only.** The guest holds only the public Drop Key half — no AMK, device key, or write-tier key. It cannot read the library, other guests' drops, or the album's contents.

## Scope

In v1: link provisioning with per-link caps (expiry, cumulative bytes, file count, per-file size, single- or multi-use) and revocation; browser drop upload of originals; a per-user inbox charged to the provisioning user's quota; adopt-in-place; discard.

Not in v1: guest-produced derivatives, metadata or provenance (the adopter produces them); guest read-back; adoption by anyone but the provisioning user (the Drop Key is escrowed only under their hierarchy); writable [share links](/design/share-links/).

## Security Contract

- **Link format.** `https://server.tld/u/{opaque_id}#{drop_pubkey}`. `{opaque_id}` follows the [share-link opaque-id rule](/design/share-links/#security-contract) (≥128-bit CSPRNG, no structure); `{drop_pubkey}` rides the fragment.
- **Drops never enter the library.** A drop lives only in the provisioning user's inbox, never on any [sync feed](/design/sync/#discovering-what-changed), and is served only to that user's devices. It carries no `AssetManifest` and never reaches [`verify_asset`](/design/cryptography/keys/#write-authorization). Library state is reachable only through adoption by a trusted client.
- **Per-link caps are enforced server-side at drop-session creation**, decided and reserved in one store operation so concurrent guests cannot both take the last slot. A leaked link is bounded to wasted quota and inbox space.
- **Quota is charged to the link owner at drop-session creation** — the single [enforcement point](/design/quota/#enforcement-points).
- **Probing reveals nothing.** An unknown, expired, revoked, or spent link and an unknown session all return one indistinguishable `404`, never `410`. A cumulative cap on a live link is the distinct `409 error.drop.cap_exhausted`, because the guest holds a real link and needs a new one.
- **Rate limits.** Drop-session creation is limited per source IP and per `{opaque_id}` (`429 error.drop.rate_limited`), in that order, before the link is resolved, failing closed. The source is resolved the way [Share Links](/design/share-links/#security-contract) resolves it.
- **Optional passphrase gates the write, not confidentiality.** The link record stores an [Argon2id](/design/cryptography/primitives/#password-based-kdf) verifier; the guest sends the derived proof as `passphrase_proof`, compared in constant time **before any cap is reserved** so wrong guesses cannot burn the link down. A refusal is `403 error.drop.passphrase_required` — not `404`, since the caller already holds the id.
- **Home-server-only.** Like a share link, an upload link is served only by the album owner's [home server](/design/federation/#album-ownership-v1-single-home-server); a peer refuses with a `{ home_server }` pointer.
- **Adopted bytes are external-origin.** Every client decodes them only in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

## Drop and Adoption Lifecycle

### 1. Provision (native client)

The client mints a [Drop Key](/design/cryptography/keys/#non-registered-accounts), escrows the private half under the master key and the user's [OGK](/design/cryptography/keys/#owner-group-keys-ogks), and registers the link record (opaque id, caps, pinned `protocol_version` and `crypto_suite_id`, optional passphrase verifier). The server never sees `{drop_pubkey}`.

### 2. Seal and upload (web client)

Per asset, the browser draws a random 32-byte key `K`, encrypts under the unchanged [STREAM construction](/design/cryptography/encryption/#stream-construction), encapsulates `K` to `{drop_pubkey}` as `kem_ct`, and uploads the ciphertext with an unsigned `DropDescriptor` (content type, plaintext size, STREAM chunk size and nonce prefix, `ciphertext_hash`, `kem_ct`, advisory `suggested_filename`). Transfer uses the [upload protocol](/design/transfer/upload/)'s chunk, finalization and [session-lifetime](/design/transfer/upload/#session-lifetime-and-discard) rules under link-capability auth. A descriptor is not a manifest: it has no signatures, no `album_id`, and no provenance; it is untrusted until a trusted client decapsulates `K` and the STREAM tags verify.

### 3. Stage (server)

The server validates the session against the link record and the [drop invariants](/design/threat-model/validation/#on-post-dopaque_id-upload-link-drop-session-and-adoption), charges quota, and stores the ciphertext as a content-addressed blob referenced by a drop-inbox row. The drop appears on the user's native clients as a [quarantine surface](/design/threat-model/scenarios/#quarantine-surfaces) awaiting review.

### 4. Review and adopt-in-place (native client, provisioning user)

The client decapsulates `K`, previews in the sandboxed decoder, and on approval:

1. Authors the [signed sidecar](/design/metadata/#sidecar-schema-v1) with an unverified guest-origin note. The guest is never a signer.
2. Wraps `K` under the destination album's AMK (`asset-keywrap/v1`) as `wrapped_file_key`; an externally chosen `K` cannot be derived, so it is carried.
3. Signs a `create` manifest with `ciphertext_hash` and `nonce_prefix` from the drop, `key_mode = wrapped`, and the **adopter** as author, and appends the `create` provenance record.
4. Submits it to `POST /v1/drops/{drop_id}/adopt`. The server re-runs the create validation battery and requires the referenced blob to be a drop in the **caller's own inbox**; only the metadata and provenance blobs are new bytes. The ciphertext hash is not recomputed — that already ran at the drop's finalization.

Server-side adoption is **claim → write asset → settle**: writing first could duplicate a photo, deleting first could lose one, so a crash leaves the row visible in the owner's inbox marked `adopting`, never lost or duplicated. The client drops its local inbox row only after the asset is written locally. Afterwards the asset is ordinary; later edits use the derived-key path, and only this `create` sets `key_mode = wrapped`.

### 5. Discard

A rejected or expired drop's inbox row is removed and its blob [garbage-collected](/design/filesystem/server/#deletion-and-garbage-collection), freeing quota. No provenance is involved.

## Why Adopt-in-Place

Re-encrypting under a derived key would upload and store every asset twice. Rewrapping the 32-byte key keeps one copy, at the cost of one contained divergence: the adopted asset's file key is carried wrapped rather than derived. It affects only key recovery — never authorization, provenance, or STREAM.

## Failure Modes

- **Leaked link:** bounded by caps, quota, rate limits, and revocation; it cannot inject into the library.
- **Server reads drops:** impossible without `{drop_pubkey}`, which it never receives.
- **Hostile file:** decoded only in the sandbox, after owner review.
- **Forged `wrapped_file_key` or mismatched `key_mode`:** covered by `device_sig` and `write_sig`; fails `verify_asset` and is quarantined.
- **Weak guest `K`:** compromises only that asset.

## Validation

- Seal → decapsulate → STREAM-decrypt round-trips byte-identically; `kem_ct` length matches the suite.
- Upload-link ids are ≥128-bit and non-sequential.
- An adopted manifest with `key_mode = wrapped` passes `verify_asset`, and a second member unwraps and decrypts the unchanged ciphertext; a forged wrapped key or mismatched `key_mode` is rejected.
- A `DropDescriptor` cannot reach any album-write path, and the inbox never appears on the sync feed.
- Caps, quota, the per-source and per-link limiters, and the indistinguishable `404` each refuse as specified.
- A crash mid-adoption leaves an `adopting` row, never a lost or duplicated asset.
