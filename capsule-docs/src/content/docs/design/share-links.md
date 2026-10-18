---
title: Share Links
description: Non-registered-account share link generation, permission model, and public-share serving
status: draft
---

Share links let a Capsule user grant view (and possibly limited write) access to an album or a specific asset *without* requiring the recipient to have a Capsule account. The recipient is the [non-registered account](/design/authentication/#account-types) class — no master key, no User IK, no MLS membership. The cryptographic shape (the link secret carries the decryption material; an optional passphrase wraps it with the [password-based KDF](/design/cryptography/primitives/#password-based-kdf)) is owned by [Cryptography — Keys: Non-registered accounts](/design/cryptography/keys/#non-registered-accounts); this doc owns everything else.

Implementation will live in the planned `capsule-server::share` module (public-share serving endpoints) and `capsule-core::sharing` (link generation, capability validation).

## Scope (v1)

In scope:

- View-only links to a single asset.
- View-only links to a whole album.
- Optional passphrase protection (the link secret + a user-chosen passphrase, both required to decrypt).
- Optional expiry (link valid until a specific timestamp).
- Revocation (publish a revocation record; the serving endpoint refuses revoked links).

Out of scope for v1 (deliberate non-goals):

- **Writable share links.** Writing requires a write-tier key + a place in the MLS group; a non-registered user has neither. Supporting writes would require an ephemeral link-scoped key hierarchy — a substantial new design that is not justified for v1. Guest *contribution* (a non-registered user depositing assets) is instead provided by [Web Upload](/design/web-upload/)'s upload links — a distinct write-to-inbox capability whose product is a staged drop, which does **not** make these view links writable.
- **Per-recipient analytics.** Link views are not tracked per-recipient. The link is the credential; the server knows it was used, not by whom.

## Security Contract

These are **normative** — the security-relevant decisions are committed; only UX presentation remains open.

- **URL format.** `https://server.tld/s/{opaque_id}#{secret}` — the secret lives in the URL **fragment**, which browsers never transmit, so the server holds only `{opaque_id}` and never the decryption secret. `{opaque_id}` is **fully opaque and carries no scope**; the asset/album scope is resolved server-side from the link record, so the URL itself leaks nothing about what it points to.
- **Opaque-id entropy.** `{opaque_id}` is a **random 128-bit value** drawn from the CSPRNG — a full 128 bits of entropy, *not* a UUIDv7 or other structured id whose embedded timestamp would cut real entropy to ~62 bits. No shorter or sequential identifier is permitted — this is the structural defense against link enumeration, independent of rate limiting.
- **Serving-endpoint rate limits.** The public serve path is rate-limited **per source IP and per `{opaque_id}`** (two independent limiters) and returns an **indistinguishable `404`** — never `410 Gone`, which would confirm a link once existed — for a not-found, revoked, or expired link alike, so probing reveals nothing and fast enumeration is throttled.
- **Passphrase unwrap is client-side.** When a passphrase protects a link, the server stores only the **wrapped** secret and never receives the passphrase: the client fetches the wrapped material and unwraps it locally via the [password-based KDF](/design/cryptography/primitives/#password-based-kdf). The server is never in the password-trust path, so a server compromise cannot brute-force passphrases beyond the [Argon2id](/design/cryptography/primitives/#password-based-kdf) cost already imposed. Because unwrap is client-side the server cannot observe a *failed* attempt, so the endpoint that returns the wrapped material is rate-limited per source IP and per `{opaque_id}` (the same limiter as the serve path); the Argon2id cost is the real brute-force backstop.
- **The privacy strip is mandatory, and it happens at issue.** A public share is by definition a boundary crossing, so the [Privacy on Export](/design/metadata/#privacy-on-export) strip (camera serial, device/session ids, GPS truncated to city level, contact tags) is applied by the **issuing client**, which seals the stripped metadata into the blob the link points at. There is **no per-share opt-out** that could leak fingerprinting fields. The serve path cannot be the enforcement point and it is important to say why rather than to leave it implied: a share's metadata is sealed under material derived from the URL fragment, which by this contract's own design never reaches the server — a server that *could* strip a share's metadata could read every share. The server's half is **containment**: a link serves **only the content addresses its own record enumerates**, so a share cannot be walked sideways into the album's unstripped metadata, even though the same store holds it. That is what makes the client's strip stick, and it is checkable server-side where a strip is not.
- **Home-server-only serving.** A share link is served **only by the album's [home server](/design/federation/#album-ownership-v1-single-home-server)**. A federated peer never serves a share; a share-scoped request at a peer returns a **structured `{ home_server }` JSON pointer** the client resolves — explicitly *not* an HTTP redirect, to avoid an open-redirect surface — never content. This keeps revocation and rate-limiting at a single authoritative point.
- **Revocation cache.** Home-server-only serving still leaves *intra-server* staleness: the serve path may run on several processes or replicas of the one home server, which consult revocation state through a **short-TTL cache (default 60 s)** rather than an authoritative read per request. The posture is fail-closed, matching the [federation revocation list](/design/federation/#token-lifecycle-and-chain-of-trust): a serving process that cannot confirm a link is still live past the TTL refuses rather than serving on stale-allowed state. (A single-process deployment reads revocation state directly and the cache is a no-op.)

**Status note.** The serve path, the owner's issue and revoke operations, and the indistinguishable `404` are **served today** by the Kynos surface, with slice `S-C4`. The containment half of the strip rule above is enforced and tested there. Two qualifications remain, recorded rather than implied:

- **The two rate limiters are absent**, not merely per-process: the counter port they need is `S-C32`'s. The `429` is deliberately *not declared* in the served contract rather than declared and unreachable. What holds without them is the defense this contract calls independent of rate limiting — the 128-bit opaque id, checked for shape at issue.
- **The revocation cache is a no-op**, exactly as the bullet above allows for a single-process deployment: the serve path resolves authoritatively on every request. The fail-closed rule is what a multi-replica cache must be written to, and the surface already has the shape it needs — a store that cannot answer is a `500`, never an implicit "live" and never the `404` that would read as "revoked".

## Contract Skeleton

The surfaces consuming code needs; the security policies they enforce are fixed by the [Security Contract](#security-contract) above.

```rust
// in capsule-core::sharing
trait ShareLinkIssuer {
    fn create_link(scope: ShareScope, expiry: Option<DateTime>, passphrase: Option<&str>) -> Result<ShareLink, Error>;
    fn revoke(link_id: ShareLinkId) -> Result<(), Error>;
}

// served by capsule-server::routes::share
//   GET  /s/{opaque_id}                 → { metadata_hash, passphrase_protected } — nothing else about the link
//   GET  /s/{opaque_id}/blob/{hash}     → octets of a blob the link names: the sealed metadata record, or ciphertext
//   GET  /s/{opaque_id}/wrapped-secret  → octets of the passphrase-WRAPPED scope, when the link is
//                                          passphrase-protected (404 otherwise); unwrap is client-side via the
//                                          password-based KDF. The passphrase itself is NEVER transmitted.
```

### The share metadata record

The blob at a link's `metadata_hash` is a **share metadata record** (`share-metadata/v1`, `capsule-core::sharing::ShareMetadata`): canonical CBOR, sealed with `seal_blob` under `HKDF(ikm=link_secret, salt=opaque_id, info="share-metadata/v1")`. The server cannot open it. It holds:

- `version`: always `share-metadata/v1`. A reader refuses any other value.
- `expires_at`: the issuer's expiry, shown to the viewer only. The server enforces expiry on its own record and never discloses it.
- `link_scope`: the link-only `WrappedScope`, present **iff** the link has no passphrase. A link with no passphrase has no wrapped secret on the server, so its scope has to travel here. A passphrase link's scope never does: in a record that the fragment alone opens, it would let the fragment open what the passphrase is meant to guard.
- `assets`: one entry per covered asset, holding only key-free parameters. They are `asset_id`, `content_hash` (the ciphertext's content address), `content_type`, plaintext `size`, `amk_version`, `nonce_prefix`, `key_mode` and `wrapped_file_key`, under the same wire rules as the signed manifest (`key_mode` is absent when `derived`, and `wrapped_file_key` is present iff `wrapped`). The record carries no capture metadata, so the strip has nothing to remove from it today. Any field that adds capture metadata crosses the boundary and must be added already stripped.

The issuing client builds the record with `publish_share`, which also yields the request's `metadata_hash` and `serves` (the record first, then each asset's ciphertext) and, for a passphrase link, the `wrapped_secret` (the canonical CBOR of the passphrase `WrappedScope`). The viewer's sequence follows from that:

1. `GET /s/{opaque_id}` gives `metadata_hash` and `passphrase_protected`.
2. `GET /s/{opaque_id}/blob/{hash}`, with `hash` = `metadata_hash`, gives the record, opened in the browser with the fragment (`capsule-wasm`'s `openShareMetadata`). A record that does not open is the same "unavailable" answer as a missing link.
3. Only if `passphrase_protected`: `GET /s/{opaque_id}/wrapped-secret`, with the passphrase prompt. Otherwise the scope is the record's `link_scope`.
4. `GET /s/{opaque_id}/blob/{hash}`, with `hash` = each asset's `content_hash`, decrypted under the opened scope.

### Issuing a link from a native client

A native client builds a link from its own library in `capsule-core` and registers it through the SDK:

1. `ShareLinkIssuer::create_link` mints the opaque id, the fragment secret and the scope encapsulation.
2. `Workspace::publish_link` reads each covered asset's record from its head manifest and signed sidecar, then calls `publish_share`. An album link covers every asset in the album that is not in trash, in asset-id order. Each entry keeps its manifest's epoch, key mode and wrapped key, because the viewer recovers each file key from the album's AMKs. An asset link carries the file key itself, so its one entry says `amk_version = 0` and `derived`, with no wrapped key. An asset in trash cannot be shared.
3. The `issue` method of `capsule_sdk::share::ShareClient` sends the publication through the generated `issue_share` operation, and `revoke` sends the generated `revoke_share`. `capsule_sdk::share::share_url` builds `{origin}/s/{opaque_id}#{fragment}`. The fragment is never in a request.

### Storing the record

The record travels in the issue request itself, as the optional `metadata_blob` member of `POST /v1/shares` (base64). It is additive, so an older client that omits it still registers a link, which serves its metadata only if the blob is already in the store. When it is present the server:

1. decodes it and refuses it with `400 error.share.malformed` if it is not base64, if it is larger than **8 MiB** decoded (`capsule_server::share::MAX_SHARE_METADATA_BYTES`), or if its SHA-256 is not `metadata_hash`;
2. admits the link as **metadata growth** under [quota](#quota), refusing with `403 error.quota.grace_locked` past the grace window. This step applies to every issue, including one that carries no record;
3. stores it under `metadata_hash` and charges it to the caller's quota, refusing with `403 error.quota.exceeded` when its new bytes would carry the account to its hard limit;
4. only then records the link, so no viewer ever resolves a link whose record is not yet servable. A link refused at this step (an opaque id already held) leaves a blob no live link names, which the collector reclaims and credits back.

**The collector keeps what a live link serves.** A record has no asset row, so the asset index counts zero references to it. The collector therefore also asks the share store whether a **live** link (not revoked, and not past its expiry) names the address in its serve set, and treats a yes as a reference. The question covers the whole serve set, not only the record, because a link may serve a blob its issuer prepared for the boundary crossing that no asset names either. Once the last live link naming a blob is revoked or expires, the blob is an ordinary orphan: it is marked on the next pass and swept after the grace window, and a link issued in that window over the same blob cancels the mark.

#### Quota

**Decision: the record counts against the owner's quota.** It is charged to the account that issues the link. The link itself is admitted as **metadata growth**, and the record's new bytes are bounded by the hard limit, as an upload's are.

- *Why charge it.* A live link keeps its record from the collector, and a link with no expiry is live until it is revoked. An uncharged record would therefore be storage an account could grow without limit by issuing links, up to 8 MiB at a time. The ledger attributes bytes by content address, so the same record under a second link is not charged again. The collector credits a record back when it sweeps the blob (`release_attribution`), as it does for every blob, so revoking the last link that names it is what returns the bytes.
- *The link is metadata growth.* Past the grace window every write that grows storage is refused, and a new link is one of them, whether or not its request carries the record. An older client that sends no record is gated the same way.
- *The record is bounded like an upload.* A record whose new bytes would carry the account to its hard limit is refused with `403 error.quota.exceeded`, the refusal an upload of the same bytes gets. Gating it only on the grace window would let an account over its limit store any number of distinct records, each publicly servable through its link, for the whole window. A record the ledger already attributes adds no bytes and is never refused, so re-sharing what is already stored still works over the limit.
- *The order.* The record is charged **after** it is stored, so a store that fails charges nothing. A record refused at the charge stays in the store uncharged and unlinked, and a charge left behind by a link that is refused afterwards is credited back. In both cases the collector reclaims the orphaned blob on its ordinary terms.
- *What would reverse it.* A deployment that wants links to be free would drop the charge and keep the grace-window gate. The cost is that the unlimited growth above is no longer bounded by quota.

**Status note.** The server side is in place: the record is uploaded with the link, and kept for as long as the link is live. `capsule_sdk::share::issue_request` sends `metadata_blob`. The `capsule share create|revoke` command and the end-to-end case are in #654.

Concrete error variants are an implementation detail; the rate-limit, opaque-id entropy, privacy-strip, and revocation policies are fixed by the [Security Contract](#security-contract) above.

## Failure Modes

- **Link enumeration.** Defeated structurally by the ≥128-bit opaque-id and operationally by per-IP/per-link rate limits with indistinguishable `404`s (see [Security Contract](#security-contract)).
- **Revoked link still served.** Home-server-only serving means a single authoritative revocation point — no peer caches a share to serve stale — and the 60 s revocation cache fails closed past its TTL.
- **Passphrase brute force.** The [Argon2id](/design/cryptography/primitives/#password-based-kdf) wrap makes weak passphrases survivable; client-side unwrap keeps the server out of the trust path; the rate-limited serve endpoint is the operational backstop.

## Validation

- **Opaque-id entropy (unit).** Assert generated ids are ≥128-bit and non-sequential; a generator producing shorter or guessable ids fails the test.
- **Enumeration resistance (smoke).** Probe the serve endpoint with random ids; assert per-IP/per-link rate limiting, and that not-found, revoked, and expired all return an indistinguishable `404`.
- **Passphrase unwrap locality (unit).** Assert the passphrase never crosses the wire — the server stores and returns only the wrapped secret; unwrap happens client-side.
- **Revocation honored (smoke).** Revoke a link; assert the serve endpoint refuses within the 60 s cache window, and fails closed past TTL when revocation state is unreachable.
- **Privacy-strip on serve (unit).** Assert the boundary-crossing field set is always stripped from the served metadata blob, with no opt-out path.
- **Home-server-only (unit).** Assert a federated peer refuses to serve a share and returns a home-server pointer.

(The validation surface grows with the client UX, but the security checks above are committed.)
