---
title: Share Links
description: Non-registered-account share link generation, permission model, and public-share serving
status: draft
---

A share link grants view access to an album or asset to someone without a Capsule account (a [non-registered account](/design/authentication/#account-types): no master key, no IK, no MLS membership). The key material is owned by [Keys — Non-registered accounts](/design/cryptography/keys/#non-registered-accounts); this doc owns the rest. Code: `capsule-core::sharing`, `capsule-server::share`, `capsule-server::routes::share`.

## Scope (v1)

View-only links to one asset or a whole album, with optional passphrase, optional expiry, and revocation.

Not in scope: **writable links** (a guest has no write key or MLS seat; guest contribution is [Web Upload](/design/web-upload/), a separate capability), and **per-recipient analytics** (the server knows a link was used, not by whom).

## Security Contract

Normative.

- **URL format.** `https://server.tld/s/{opaque_id}#{secret}`. The secret is in the **fragment** and never reaches the server. `{opaque_id}` carries no scope; scope is resolved from the server's record.
- **Opaque-id entropy.** `{opaque_id}` MUST be a **random 128-bit** CSPRNG value, never a UUIDv7 or other structured or sequential id. This defeats enumeration independently of rate limiting.
- **One `404`.** Not-found, revoked, expired, malformed id, and a blob the link does not name all return the same `404` (never `410`). The public path discloses nothing about the link: no owner, album, scope or expiry.
- **Rate limits.** The serve path is limited **per `{opaque_id}`** and **per source IP**, charged before the link is resolved, failing closed. The per-source budget is charged first, so a source that has spent it cannot drain a link's budget. Both answer `429 error.share.rate_limited` with a retry instant. The source is the client address resolved through the deployment's trusted proxies (`TRUSTED_PROXIES`, `TRUSTED_PROXY_HOPS`): a forwarding header is believed only from a hop the deployment named, because an untrusted one would let the attacker choose the key. An IPv6 source is its `/64`.
- **Passphrase unwrap is client-side.** The server stores only the **wrapped** secret and never receives the passphrase; the client unwraps with the [password-based KDF](/design/cryptography/primitives/#password-based-kdf). The wrapped-secret endpoint shares the serve-path limits; the Argon2id cost is the brute-force backstop.
- **The privacy strip happens at issue.** The issuing client applies the [Privacy on Export](/design/metadata/#privacy-on-export) strip before sealing; there is **no per-share opt-out**. The server cannot strip, because share metadata is sealed under the fragment it never sees. The server's half is **containment**: a link serves **only the content addresses its own record enumerates**.
- **Home-server-only serving.** Only the album's [home server](/design/federation/#album-ownership-v1-single-home-server) serves a share. A peer answers with a structured `{ home_server }` JSON pointer, never an HTTP redirect (open-redirect risk) and never content.
- **Revocation cache.** Multi-replica serving may cache revocation state for at most **60 s** and MUST fail closed past the TTL, like the [federation revocation list](/design/federation/#token-lifecycle-and-chain-of-trust). A store that cannot answer is `500`, never "live" and never the `404`. A single-process deployment (today's) reads revocation state directly.

## Contract Skeleton

```text
GET  /s/{opaque_id}                 → { metadata_hash, passphrase_protected }
GET  /s/{opaque_id}/blob/{hash}     → a blob the link names (the sealed record, or ciphertext); Range supported
GET  /s/{opaque_id}/wrapped-secret  → passphrase-wrapped scope, or 404 if the link has no passphrase
POST /v1/shares                     → issue (owner)
DELETE /v1/shares/{opaque_id}       → revoke (owner)
```

### The share metadata record

The blob at `metadata_hash` is a `share-metadata/v1` record: canonical CBOR, sealed under `HKDF(ikm=link_secret, salt=opaque_id, info="share-metadata/v1")`. The server cannot open it. Fields:

- `version`: readers refuse any other value.
- `expires_at`: for display only; the server enforces expiry from its own record.
- `link_scope`: the scope, present **iff** the link has no passphrase. A passphrase link's scope MUST NOT appear here, since the fragment alone opens this record.
- `assets`: per-asset key-free parameters (`asset_id`, `content_hash`, `content_type`, `size`, `amk_version`, `nonce_prefix`, `key_mode`, `wrapped_file_key`) under the manifest's wire rules. It carries no capture metadata; any field that adds some MUST be added already stripped.

Viewer sequence: fetch the link → fetch and open the record with the fragment (a record that does not open is "unavailable", like a missing link) → if `passphrase_protected`, fetch the wrapped secret and prompt → fetch and decrypt each asset's `content_hash`.

### Issuing a link from a native client

The client mints the opaque id, fragment secret and scope; builds the record from each asset's head manifest and sidecar; and issues it through the generated SDK operation. An album link covers every non-trashed asset; an asset link carries the file key itself. Trashed assets cannot be shared. The fragment is never in a request. CLI: `capsule share create|revoke`.

### Storing the record

The record travels as the optional base64 `metadata_blob` of `POST /v1/shares` (optional so older clients still issue). The server, in order:

1. rejects `400 error.share.malformed` if it is not base64, exceeds **8 MiB**, or does not hash to `metadata_hash`;
2. admits the link as metadata growth under [quota](#quota) (`403 error.quota.grace_locked` past grace), with or without a record;
3. charges the record, refusing `403 error.quota.exceeded` at the hard limit, and only then stores it;
4. only then records the link, so no viewer resolves a link whose record is not servable.

**Garbage collection.** The collector treats any blob in a **live** link's serve set as referenced. When the last such link is revoked or expires, the blob becomes an ordinary orphan.

#### Quota

The record counts against the issuing account's quota; otherwise unexpiring links would be unbounded free storage. Bytes are attributed per content address, so re-sharing is free and is never refused over the limit. The charge precedes the store, so a refused record never reaches it; the charge and store run to completion even if the request is cancelled. A crash between them leaves an uncredited charge, accepted as bounded by the hard limit.

## Failure Modes

- **Enumeration:** 128-bit ids, rate limits, one `404`.
- **Revoked link served:** single authoritative home server; cache fails closed.
- **Passphrase brute force:** Argon2id, client-side unwrap, rate-limited endpoint.

## Validation

- **Opaque-id entropy (unit):** ids are 128-bit and non-sequential.
- **Enumeration (smoke):** not-found, revoked and expired give an identical `404`; limits apply.
- **Client address (integration):** a spoofed `X-Forwarded-For` from an untrusted peer is ignored, a trusted hop's is honoured, and the per-source budget trips independently of the per-link one.
- **Passphrase locality (unit):** the passphrase never crosses the wire.
- **Revocation (smoke):** honored within the cache window; fails closed when state is unreachable.
- **Strip and containment (unit):** the issued record carries no boundary-crossing fields; a link serves nothing outside its record.
- **Home-server-only (unit):** a peer returns a pointer, never content.
