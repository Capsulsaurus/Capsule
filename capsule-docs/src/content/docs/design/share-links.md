---
title: Share Links
description: Non-registered-account share link generation, permission model, and public-share serving
status: draft
---

A share link grants view access to an album or asset to someone without a Capsule account, a [non-registered account](/design/authentication/#account-types) with no master key, IK or MLS membership. [](/design/cryptography/keys/#non-registered-accounts) owns the key material and this doc owns the rest. Code: `capsule-core::sharing`, `capsule-server::share`, `capsule-server::routes::share`.

## Scope (v1)

View-only links to one asset or a whole album, with optional passphrase, optional expiry and revocation.

Out of scope are **writable links**, since a guest has no write key or MLS seat (guest contribution is [](/design/web-upload/), a separate capability), and **per-recipient analytics**, since the server knows a link was used, not by whom.

## Security Contract

Normative.

- **URL format.** `https://server.tld/s/{opaque_id}#{secret}`. The secret is in the **fragment** and never reaches the server. `{opaque_id}` carries no scope, which is resolved from the server's record.
- **Opaque-id entropy.** `{opaque_id}` MUST be a **random 128-bit** CSPRNG value, never a UUIDv7 or other structured or sequential id. This defeats enumeration independently of rate limiting.
- **One `404`.** Not-found, revoked, expired, a malformed id and a blob the link does not name all return the same `404`, never `410`. The public path discloses nothing about the link: no owner, album, scope or expiry.
- **Rate limits.** The serve path is limited **per `{opaque_id}`** and **per source IP**, charged before the link is resolved and failing closed. The per-source budget is charged first, so a source that has spent it cannot drain a link's budget. Both answer `429 error.share.rate_limited` with a retry instant. An IPv6 source is its `/64`.
- **Source address.** The source is the client address resolved through the deployment's trusted proxies (`TRUSTED_PROXIES`, `TRUSTED_PROXY_HOPS`). A forwarding element is believed only when the hop that sent it is one the deployment named, because an untrusted one would let the attacker choose the key. Exactly one forwarding field is read, the one `TRUSTED_PROXY_HEADER` names (`X-Forwarded-For` by default, or `Forwarded`), and it holds only if each named proxy appends to that field itself. The other field is ignored, so a named proxy that passes a client's copy of it through unchanged vouches for nothing ([](/guides/self-hosting/#configure)).
- **Passphrase unwrap is client-side.** The server stores only the **wrapped** secret and never receives the passphrase; the client unwraps with the [password-based KDF](/design/cryptography/primitives/#password-based-kdf). The wrapped-secret endpoint shares the serve-path limits, and the Argon2id cost is the brute-force backstop.
- **The privacy strip happens at issue.** The issuing client applies the [Privacy on Export](/design/metadata/#privacy-on-export) strip to every covered sidecar before sealing. The default strips every row. An opt-in is chosen for that one link and never stored, so the next link is strict again. The server cannot strip, because share metadata is sealed under the fragment it never sees; its half is **containment**: a link serves **only the content addresses its own record enumerates**.
- **Home-server-only serving.** Only the album's [home server](/design/federation/#album-ownership-v1-single-home-server) serves a share. A peer answers with a structured `{ home_server }` JSON pointer, never an HTTP redirect (open-redirect risk) and never content.
- **Revocation cache.** Multi-replica serving may cache revocation state for at most **60 s** and MUST fail closed past the TTL, like the [federation revocation list](/design/federation/#token-lifecycle-and-chain-of-trust). A store that cannot answer is `500`, never "live" and never the `404`. A single-process deployment (today's) reads revocation state directly.

## Contract Skeleton

The public serve path is `GET /s/{opaque_id}`, which answers `{ metadata_hash, passphrase_protected }`, plus the blobs the link names (the sealed record or ciphertext, `Range` supported) and the passphrase-wrapped scope (`404` when the link has no passphrase). The owner issues and revokes links under `/v1/shares`. Shapes are in [](/reference/api/shares/).

### The share metadata record

The blob at `metadata_hash` is a `share-metadata/v1` record: canonical CBOR sealed under `HKDF(ikm=link_secret, salt=opaque_id, info="share-metadata/v1")`, which the server cannot open. Fields:

- `version`: readers refuse any other value.
- `expires_at`: for display only; the server enforces expiry from its own record.
- `link_scope`: the scope, present **iff** the link has no passphrase. A passphrase link's scope MUST NOT appear here, since the fragment alone opens this record.
- `assets`: per-asset key-free parameters (`asset_id`, `content_hash`, `content_type`, `size`, `amk_version`, `nonce_prefix`, `key_mode`, `wrapped_file_key`) under the manifest's wire rules, and an optional `sidecar`.
- `assets[].sidecar`: the canonical CBOR of the asset's sidecar **after** the [Privacy on Export](/design/metadata/#privacy-on-export) strip, and the only place capture metadata travels. It is unsigned once anything was stripped, because the library signature covered the removed fields. It is wire-absent when the issuer sent none, so the field is additive within `share-metadata/v1` and a reader that predates it ignores it. A reader refuses a sidecar that does not decode or that names another asset. Any other field that adds capture metadata MUST be added already stripped.

Viewer sequence:

1. Fetch the link.
2. Fetch and open the record with the fragment. A record that does not open is "unavailable", like a missing link.
3. If `passphrase_protected`, fetch the wrapped secret and prompt.
4. Fetch and decrypt each asset's `content_hash`.

### Issuing a link from a native client

The client mints the opaque id, fragment secret and scope, builds the record from each asset's head manifest and sidecar, and issues it through the generated SDK operation. An album link covers every non-trashed asset; an asset link carries the file key itself. Trashed assets cannot be shared. The fragment is never in a request. CLI: `capsule share create|revoke`.

### Storing the record

The record travels as the optional base64 `metadata_blob` of `POST /v1/shares`, optional so older clients still issue. The server, in order:

1. rejects `400 error.share.malformed` if it is not base64, exceeds **8 MiB**, or does not hash to `metadata_hash`;
2. admits the link as metadata growth under [quota](#quota) (`403 error.quota.grace_locked` past grace), with or without a record;
3. charges the record, refusing `403 error.quota.exceeded` at the hard limit, and only then stores it;
4. only then records the link, so no viewer resolves a link whose record is not servable.

**Garbage collection.** The collector treats any blob in a **live** link's serve set as referenced. When the last such link is revoked or expires, the blob becomes an ordinary orphan.

#### Quota

The record counts against the issuing account's quota, or unexpiring links would be unbounded free storage. Bytes are attributed per content address, so re-sharing is free and never refused over the limit. The charge precedes the store, so a refused record never reaches it, and both run to completion even if the request is cancelled. A crash between them leaves an uncredited charge, accepted as bounded by the hard limit.

## Failure Modes

- **Enumeration:** 128-bit ids, rate limits, one `404`.
- **Revoked link served:** single authoritative home server; the cache fails closed.
- **Passphrase brute force:** Argon2id, client-side unwrap, rate-limited endpoint.

## Validation

- **Opaque-id entropy (unit):** ids are 128-bit and non-sequential.
- **Enumeration (smoke):** not-found, revoked and expired give an identical `404`; limits apply.
- **Client address (integration):** a spoofed `X-Forwarded-For` from an untrusted peer is ignored, a trusted hop's is honoured, and the per-source budget trips independently of the per-link one.
- **Passphrase locality (unit):** the passphrase never crosses the wire.
- **Revocation (smoke):** honoured within the cache window; fails closed when state is unreachable.
- **Strip and containment (unit):** the recipient-opened record carries no boundary-crossing field, the library sidecar is unchanged, and an opt-in reaches one link only; a link serves nothing outside its record.
- **Home-server-only (unit):** a peer returns a pointer, never content.
