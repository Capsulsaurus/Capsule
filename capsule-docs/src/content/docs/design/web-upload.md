---
title: Web Upload
description: Browser-based guest drops — upload-link provisioning, the sealed drop format, and adoption into the library
---

Web upload lets a Capsule user accept assets from someone who has **no Capsule account** — a guest with a browser, the *web client*. Unlike a native client, the web client holds no album keys, has no place in any album's MLS group, and (being WASM in a browser) cannot run the full signed-import pipeline. So it does not write into the library directly. Instead the user **provisions an upload link**; the guest's browser **encrypts each asset client-side and seals the asset key to the user's public key**; the sealed bytes land in a **staging inbox** charged to the provisioning user's quota; and nothing becomes a library asset until the user **reviews and adopts it on one of their native (trusted) clients**, where provenance is finally appended. The guest is the [non-registered account](/design/authentication/#account-types) class — no master key, no User IK, no MLS membership.

This is the realization of the [Keys — Non-registered accounts](/design/cryptography/keys/#non-registered-accounts) writing path: an ephemeral, link-scoped key (the **Drop Key**) lets a guest *contribute to an inbox* without ever being able to write into an album. It is the inbound counterpart of [share links](/design/share-links/) (outbound view access): the same opaque-id, fragment-secret, and revocation model, applied to a write-to-inbox capability rather than a read one.

This doc **owns** the upload-link capability, the sealed **drop** wire object, the drop upload protocol, the adoption transition, and the **web client class**. The Drop Key's cryptographic shape and escrow are owned by [Keys — Non-registered accounts](/design/cryptography/keys/#non-registered-accounts); the in-place key-rewrap on adoption is owned by [Keys — Key Chain](/design/cryptography/keys/#key-chain) and [Encryption — Asset Key Derivation](/design/cryptography/encryption/#asset-key-derivation); the `key_mode`/`wrapped_file_key` manifest fields are owned by [Provenance — Asset Manifest](/design/cryptography/provenance/#asset-manifest). This doc references those declarations and restates none of them.

Implementation will live in `capsule-core::drop` (drop sealing in WASM, link issuance, adoption rewrap), `capsule-api-media::drops` (drop store, the provisioning user's inbox, the adoption transition), and `capsule-web` (the browser/WASM client). The drop upload reuses the wire mechanics of [`capsule-api-upload`](/design/import/upload-protocol/); adoption is driven through `capsule-sdk`.

## Two Confidentiality Properties

Because the uploader is a non-member yet the bytes are charged to a registered user, web upload must hold two distinct confidentiality properties. Both are normative and are stated here so there is no interpretation gap:

1. **Server-blind.** The server stores only ciphertext plus a **KEM-encapsulated key it cannot open**. The encapsulation public key (the Drop Key public half) is delivered to the guest in the link's **URL fragment**, which browsers never transmit, so the server never sees it and cannot substitute a key it controls to read drops. This is the same fragment-secret guarantee [share links](/design/share-links/#security-contract) rely on, applied to an *encapsulation* key rather than a decryption secret.
2. **Contribute-only.** The web client gets **no read access** to the album or any existing library content. It holds only the link's *public* Drop Key half — never an AMK, never a device key, never a write-tier key — so it can deposit sealed bytes and nothing more. A guest cannot read what is already in the library, cannot read other guests' drops, and cannot enumerate the album.

## Scope (v1)

In scope:

- **Upload-link provisioning** by a registered user's native client, with per-link caps (expiry, max cumulative bytes, max file count, max single-file size, single-use or multi-use) and revocation.
- **Browser drop upload** of original asset bytes, sealed client-side under a fresh per-asset key encapsulated to the Drop Key.
- **A per-user staging inbox** of pending drops, charged to the provisioning user's quota, surfaced on that user's native clients for review.
- **Adopt-in-place**: on the user's approval, a native client turns a pending drop into a library asset *without re-uploading the bytes*, by rewrapping the guest's asset key under the album AMK.
- **Discard**: a pending drop the user rejects (or that expires) is deleted; its bytes are GC'd and the quota is freed.

Out of scope for v1 (deliberate non-goals):

- **Guest-generated derivatives, metadata edits, or provenance.** A drop carries original bytes and a minimal unsigned descriptor only. Thumbnails, previews, the signed sidecar, and the provenance chain are produced by the adopting native client — the guest cannot sign anything ([Keys — Write Authorization](/design/cryptography/keys/#write-authorization)).
- **Guest read-back / two-way exchange.** An upload link is write-only; it grants no view access. Sharing *out* to a guest is [share links](/design/share-links/), a separate capability.
- **Adoption by a co-owner who is not the provisioning user.** The Drop Key is escrowed only under the provisioning user's hierarchy, so only that user's own devices can decapsulate and adopt. Handing drops to a co-owner is a later extension, not v1.
- **Writable share links.** Web upload does **not** make [share links](/design/share-links/) writable; it is a distinct capability whose product is a staged drop, never a direct album write.

## Security Contract

These are **normative** — the security-relevant decisions are committed; only UX presentation remains open.

- **Upload-link URL format.** `https://server.tld/u/{opaque-id}#{drop_pubkey}`. `{opaque-id}` is a **random 128-bit value** from the CSPRNG — full 128-bit entropy, *not* a UUIDv7 or other structured id (identical rule to the [share-link opaque-id](/design/share-links/#security-contract)); it is fully opaque and carries no scope. `{drop_pubkey}` is the Drop Key public half, carried in the **fragment** so the server never receives it (see [Server-blind](#two-confidentiality-properties)).
- **Drops never enter the library.** A drop is written only to the provisioning user's **inbox**; it is never an album asset, never appears in any album member's [`/sync`](/design/import/download-sync/#discovering-what-changed) feed, and is served only to the provisioning user's own authenticated devices. A drop carries **no `AssetManifest`** — no `device_sig`, no `write_sig`, no `album_id`, no provenance — and therefore never flows through [`verify_asset`](/design/cryptography/keys/#write-authorization). Library state is reachable only through adoption by a trusted client.
- **Per-link caps are enforced server-side at the no-key layer.** Expiry, cumulative-byte cap, file-count cap, and per-file size cap are checked on every drop-session creation; an over-cap or expired/revoked link is refused. These bound a leaked link to *wasted quota and inbox space*, never to library corruption.
- **Quota is charged to the provisioning user at drop-session creation.** A drop debits the link owner's quota at session creation — the single hard [enforcement point](/design/quota/#enforcement-points) — using the [`upload_user_id = owner_id` attribution](/design/quota/#accounting-model). A link cannot be used to push the owner past their hard limit.
- **Serving-endpoint rate limits.** Drop-session creation is rate-limited **per source IP and per `{opaque-id}`** (two independent limiters), and a not-found, revoked, or expired link returns an **indistinguishable `404`** — never `410 Gone` — exactly as the [share-link serve path](/design/share-links/#security-contract), so probing reveals nothing.
- **Optional passphrase is an abuse gate, not a confidentiality layer.** A link may carry an optional passphrase (wrapped via the [password-based KDF](/design/cryptography/primitives/#password-based-kdf)); the guest must supply it to open a drop session. It limits *who may spend the owner's quota*; it adds no confidentiality, because the guest already encrypts every asset. The passphrase is verified the same client-out-of-band way share links use, never transmitted in the clear.
- **Home-server-only.** Like a [share link](/design/share-links/#security-contract), an upload link is served **only by the album owner's [home server](/design/federation/#album-ownership-v1-single-home-server)**; a federated peer never accepts a drop and returns a structured `{ home_server }` pointer the client resolves. This keeps revocation, rate-limiting, and quota at one authoritative point.
- **Adopted bytes are external-origin.** No device authored a drop's plaintext, so an adopted asset is **never** "local-origin": every client (including the adopter, on preview) decodes its bytes only in the [sandboxed decoder](/design/clients/#sandboxed-decoder). A hostile or malformed guest file can at worst crash a sandbox; it cannot reach the host or be silently admitted.

## Drop and Adoption Lifecycle

### 1. Provision (native client, registered user)

The user's client mints a [Drop Key](/design/cryptography/keys/#non-registered-accounts) (a hybrid X25519 + ML-KEM-768 KEM keypair), wraps the private half under the [account master key](/design/cryptography/keys/#registered-accounts) and re-wraps it to the user's [OGK](/design/cryptography/keys/#owner-group-keys-ogks) so any of the user's enrolled devices can later decapsulate, and registers an upload-link record with the server: `{opaque-id}`, the per-link caps, the pinned `protocol_version` + `crypto_suite_id`, and an optional passphrase wrap. The server stores the record under `{opaque-id}` and never sees `{drop_pubkey}`. The user shares the full URL (including the fragment) with the guest over any out-of-band channel.

### 2. Seal and upload (web client, guest)

For each selected asset the web client:

1. Draws a random 32-byte asset key **`K`** from the browser CSPRNG.
2. Encrypts the asset with **AES-256-GCM-STREAM** under `K`, using the *unchanged* [STREAM construction](/design/cryptography/encryption/#stream-construction) (65,520-byte plaintext chunks, a fresh 7-byte `nonce_prefix`), and computes `ciphertext_hash` incrementally.
3. **Encapsulates `K`** to `{drop_pubkey}` with the link's KEM, producing `kem_ct`.
4. Emits an unsigned **`DropDescriptor`** and uploads it alongside the ciphertext via the drop upload protocol — the [upload protocol](/design/import/upload-protocol/)'s chunk and finalization mechanics under link-capability auth, with the drop endpoints in the [Contract Skeleton](#contract-skeleton):

```rust
DropDescriptor {
  content_type:       enum,          // closed enum for the link's protocol_version (same set as a manifest's)
  plaintext_size:     u64,
  chunk_size:         u32,           // 65,520
  nonce_prefix:       [u8; 7],       // the STREAM nonce prefix used above
  ciphertext_hash:    bytes,         // content-address digest of the STREAM ciphertext
  kem_ct:             bytes,         // K encapsulated to {drop_pubkey}; length fixed by crypto_suite_id
  suggested_filename: Option<String>, // guest-supplied, unverified; advisory only
}
```

A `DropDescriptor` is deliberately **not** an [`AssetManifest`](/design/cryptography/provenance/#asset-manifest): it has no signatures, no `album_id`, no `amk_version`, and no provenance link. Its integrity is established only when a trusted client decapsulates `K` and the STREAM tags verify; until then it is opaque, untrusted bytes in an inbox.

### 3. Stage (server)

The server validates the drop session against the link record and the no-key drop invariants ([Threat Model — On `POST /drop`](/design/threat-model/validation/#server-side-validation-invariants)), debits the provisioning user's quota, and stores the ciphertext as a content-addressed blob in the [blob store](/design/filesystem/server/) referenced by a **drop-inbox row** (not an album asset row), with the `DropDescriptor` attached. The drop now appears in the provisioning user's inbox and on their native clients as "awaiting your review" — a [quarantine surface](/design/threat-model/scenarios/#quarantine-surfaces), never silently applied.

### 4. Review and adopt-in-place (native client, provisioning user)

The user fetches a pending drop, decapsulates `kem_ct` with the Drop Key private half (unwrapped from master-key/OGK escrow) to recover `K`, decrypts in the [sandboxed decoder](/design/clients/#sandboxed-decoder), and previews it. On approval the client adopts the drop into a chosen album **without re-uploading the bytes**:

1. Assign a `file_id` and author the [signed sidecar](/design/metadata/#sidecar-schema-v1), including an **unverified, self-asserted guest-origin note** (`received via link {opaque-id} on {date}`, optional `suggested_filename`). This note is descriptive provenance only; the guest is **never** a signer.
2. **Rewrap `K` under the destination album's AMK** with the [`asset-keywrap/v1`](/design/cryptography/encryption/#asset-key-derivation) derivation, producing `wrapped_file_key`. Because `K` was chosen by an external party it cannot be re-derived from the AMK — it is *carried* wrapped instead.
3. Build an `AssetManifest` with `action = create`, `ciphertext_hash = drop.ciphertext_hash`, `nonce_prefix = drop.nonce_prefix`, the freshly authored `metadata_blob_hash`, `key_mode = wrapped`, and `wrapped_file_key`; set `created_by_user`/`created_by_device` to the **adopter** (the cryptographic author); sign `device_sig` + `write_sig` and append the `create` provenance record. See [Provenance — Asset Manifest](/design/cryptography/provenance/#asset-manifest).
4. Submit the `create` write. Its `ciphertext_hash` references the **already-stored drop blob**, so only the small metadata blob is uploaded. The server validates the manifest envelope (invariants 1–8, 16–18, 25) **and** that the referenced blob is a drop in the caller's own inbox, then **atomically promotes** the blob from inbox to album asset — writing the asset row, the provenance record, and the refcount — and deletes the inbox row, in one transaction. The bulk bytes never move; the quota is unchanged (same user).

From this point the asset is an ordinary library asset: it syncs, it `verify_asset`-accepts on every other album member's device, and any later edit (`replace`, `metadata-update`) follows the standard **derived-key** path. `key_mode = wrapped` is set only by this adopting `create`.

### 5. Discard

A drop the user rejects, or one whose link expires before adoption, is deleted: the inbox row is removed and the blob's reference is dropped, so it is [garbage-collected](/design/filesystem/server/#deletion-and-garbage-collection) and the provisioning user's quota is freed. Discarding requires no provenance, because the drop was never a library asset.

## Why Adopt-in-Place

The alternative — decrypt the drop, re-encrypt it under the AMK with a *derived* key, and re-upload — would keep the crypto core's "every file key is `HKDF(AMK, file_id‖nonce_prefix)`" invariant untouched, but it would upload each asset a second time and double its storage for the duration of the window. Adopt-in-place keeps the guest's ciphertext and rewraps only the 32-byte key, so the bytes are uploaded once and stored once. The cost is a single, bounded divergence: an adopted asset's file key is **carried wrapped** rather than **derived**, signalled by the closed `key_mode` field and recovered by unwrapping `wrapped_file_key` with the AMK ([Keys — Key Chain](/design/cryptography/keys/#key-chain)). The divergence is contained — it touches only decryption-key recovery, never authorization (the adopter's `write_sig` is the sole authority either way), never the provenance chain, and never the STREAM construction.

## Contract Skeleton

The surfaces consuming code needs; the security policies they enforce are fixed by the [Security Contract](#security-contract) above.

```rust
// in capsule-core::drop  (link issuance + adoption; runs on native clients)
trait UploadLinkIssuer {
    fn create_link(album_hint: Option<AlbumId>, caps: LinkCaps, passphrase: Option<&str>) -> Result<UploadLink, Error>;
    fn revoke(link_id: UploadLinkId) -> Result<(), Error>;
}

struct LinkCaps {
    expires_at:        Option<DateTime>,
    max_total_bytes:   Option<u64>,
    max_file_count:    Option<u32>,
    max_file_size:     Option<u64>,
    single_use:        bool,
}

trait DropAdopter {
    fn list_inbox() -> Result<Vec<PendingDrop>, Error>;
    fn adopt(drop_id: DropId, into_album: AlbumId) -> Result<AssetId, Error>; // decapsulate K → rewrap under AMK → create
    fn discard(drop_id: DropId) -> Result<(), Error>;
}

// in capsule-core::drop  (sealing; compiled to WASM for capsule-web)
fn seal_drop(plaintext: impl Read, drop_pubkey: KemPublicKey, crypto_suite_id: u16) -> Result<SealedDrop, Error>;

// in capsule-api-media::drops
//   POST   /u/{opaque-id}/drop            → open a drop session (link-capability auth; quota + caps checked here)
//   PATCH  /u/{opaque-id}/drop/{id}       → append a chunk (reuses upload-protocol chunk rules)
//   GET    /drops                         → provisioning user's inbox (session-token auth)
//   POST   /drops/{id}/adopt              → create-manifest write referencing the inbox blob; atomic promotion
//   DELETE /drops/{id}                    → discard a pending drop
```

Concrete error variants are an implementation detail; the opaque-id entropy, fragment-delivered Drop Key, per-link caps, quota-at-creation, rate-limit, and no-library-injection policies are fixed by the [Security Contract](#security-contract).

## Failure Modes

- **Leaked upload link.** Anyone with the URL can spend the owner's quota and fill their inbox. Bounded, not eliminated: per-link caps (bytes, count, size, expiry) + quota refusal at drop-session creation + per-IP/per-link rate limits + one-tap revocation cap the damage at *wasted quota and inbox clutter*; a leaked link can never inject into the library, because adoption requires the owner's trusted client.
- **Server tries to read drops.** The encapsulation key lives in the URL fragment and reaches only the guest's browser; the server holds `kem_ct` (which it cannot open) and the opaque-id. A server that substitutes its own public key cannot, because it never sees `{drop_pubkey}` to splice into the link the guest already holds.
- **Hostile or malformed guest file.** Adopted bytes are external-origin and always decoded in the [sandboxed decoder](/design/clients/#sandboxed-decoder); the owner reviews before adoption and can discard unreviewed. A decoder CVE crashes a sandbox, not the app.
- **Forged `wrapped_file_key` on an adopted asset.** The field lives in the signed `AssetManifest` and is covered by `device_sig` + `write_sig`; a tampered wrapped key, or a `key_mode` that disagrees with the manifest, fails [`verify_asset`](/design/cryptography/keys/#write-authorization) and is quarantined.
- **Weak or reused guest key `K`.** `K` protects only its own drop and is never derived from the AMK, so a weak `K` compromises at most that one asset's bytes — never the AMK, the album, or any other asset. Confidentiality is per-asset.

## Validation

The drop sealing and adoption rewrap live in `capsule-core::drop` (so they apply uniformly to the web client and native clients); the server drop store + inbox + adoption transition live in `capsule-api-media::drops`.

- **Drop seal round-trip (unit).** Seal a plaintext under a random `K` to a Drop Key public half; decapsulate with the private half; STREAM-decrypt; assert byte-equality. Assert `kem_ct` length matches the suite.
- **Opaque-id entropy (unit).** Assert generated upload-link ids are ≥128-bit and non-sequential, identical to the [share-link check](/design/share-links/#validation).
- **Adoption rewrap accepts (unit).** Decapsulate a drop, rewrap `K` under a test AMK, build the `create` manifest with `key_mode = wrapped`; assert [`verify_asset`](/design/cryptography/keys/#write-authorization) accepts and a second member can unwrap `wrapped_file_key` and STREAM-decrypt the unchanged ciphertext.
- **Wrapped-mode negative cases (unit).** A manifest with `key_mode = wrapped` but a forged `wrapped_file_key` (failing the `metadata_blob_hash` binding), or `key_mode` disagreeing with the manifest, is rejected at `verify_asset`. Owned alongside the [Provenance negative-case suite](/design/cryptography/provenance/#validation).
- **No library injection (unit).** Assert a `DropDescriptor` cannot be presented on any album-write path: the drop endpoints accept no `album_id`/manifest fields, and the inbox is never emitted on `/sync`.
- **Drop session lifecycle (smoke).** Against a real Postgres + blob store: open a drop session, exceed each per-link cap in turn, exhaust the owner's quota, and probe a revoked/expired/unknown link — assert caps and quota refuse, the rate limiter engages, and not-found/revoked/expired all return an indistinguishable `404`.
- **Adoption atomicity (smoke).** Inject a crash between blob promotion and the Postgres commit; assert the asset row, provenance record, and inbox-row deletion either all land or all roll back — no half-adopted drop, no orphaned blob, no zombie inbox row.

The end-to-end path — web client seals a drop → the provisioning user adopts on a native client → the asset appears in the library and `verify_asset`-accepts on a second device — is bounded E2E surface, listed in [Module Map](/design/module-map/#e2e-test-surface).
