---
title: Share Links
description: Non-registered-account share link generation, permission model, and public-share serving
---

Share links let a Capsule user grant view (and possibly limited write) access to an album or a specific asset *without* requiring the recipient to have a Capsule account. The recipient is the [non-registered account](/design/authentication/#account-types) class — no master key, no User IK, no MLS membership. The cryptographic shape (the link secret carries the decryption material; an optional passphrase wraps it with the [password-based KDF](/design/cryptography/primitives/#password-based-kdf)) is owned by [Cryptography — Keys: Non-registered accounts](/design/cryptography/keys/#non-registered-accounts); this doc owns everything else.

Implementation will live in `capsule-api-media::shares` (public-share serving endpoints) and `capsule-core::sharing` (link generation, capability validation).

## Scope (v1)

In scope:

- View-only links to a single asset.
- View-only links to a whole album.
- Optional passphrase protection (the link secret + a user-chosen passphrase, both required to decrypt).
- Optional expiry (link valid until a specific timestamp).
- Revocation (publish a revocation record; the serving endpoint refuses revoked links).

Out of scope for v1 (deliberate non-goals):

- **Writable share links.** Writing requires a write-tier key + a place in the MLS group; a non-registered user has neither. Supporting writes would require an ephemeral link-scoped key hierarchy — a substantial new design that is not justified for v1.
- **Per-recipient analytics.** Link views are not tracked per-recipient. The link is the credential; the server knows it was used, not by whom.

## Security Contract

These are **normative** — the security-relevant decisions are committed; only UX presentation remains open.

- **URL format.** `https://server.tld/s/{opaque-id}#{secret}` — the secret lives in the URL **fragment**, which browsers never transmit, so the server holds only `{opaque-id}` and never the decryption secret. `{opaque-id}` is **fully opaque and carries no scope**; the asset/album scope is resolved server-side from the link record, so the URL itself leaks nothing about what it points to.
- **Opaque-id entropy.** `{opaque-id}` is a **random 128-bit value** drawn from the CSPRNG — a full 128 bits of entropy, *not* a UUIDv7 or other structured id whose embedded timestamp would cut real entropy to ~62 bits. No shorter or sequential identifier is permitted — this is the structural defense against link enumeration, independent of rate limiting.
- **Serving-endpoint rate limits.** The public serve path is rate-limited **per source IP and per `{opaque-id}`** (two independent limiters) and returns an **indistinguishable `404`** — never `410 Gone`, which would confirm a link once existed — for a not-found, revoked, or expired link alike, so probing reveals nothing and fast enumeration is throttled.
- **Passphrase unwrap is client-side.** When a passphrase protects a link, the server stores only the **wrapped** secret and never receives the passphrase: the client fetches the wrapped material and unwraps it locally via the [password-based KDF](/design/cryptography/primitives/#password-based-kdf). The server is never in the password-trust path, so a server compromise cannot brute-force passphrases beyond the [Argon2id](/design/cryptography/primitives/#password-based-kdf) cost already imposed. Because unwrap is client-side the server cannot observe a *failed* attempt, so the endpoint that returns the wrapped material is rate-limited per source IP and per `{opaque-id}` (the same limiter as the serve path); the Argon2id cost is the real brute-force backstop.
- **Privacy strip on serve is mandatory.** The serve path **always** applies the boundary-crossing strip from [Metadata — Privacy on Export](/design/metadata/#privacy-on-export) (camera serial, device/session ids, GPS truncated to city level, contact tags). There is **no per-share opt-out** that could leak fingerprinting fields — a public share is, by definition, a boundary crossing.
- **Home-server-only serving.** A share link is served **only by the album's [home server](/design/federation/#album-ownership-v1-single-home-server)**. A federated peer never serves a share; a share-scoped request at a peer returns a **structured `{ home_server }` JSON pointer** the client resolves — explicitly *not* an HTTP redirect, to avoid an open-redirect surface — never content. This keeps revocation and rate-limiting at a single authoritative point.
- **Revocation cache.** Per-link revocation is checked against a **short-TTL cache (default 60 s)** with the same fail-closed posture as the [federation revocation list](/design/federation/#token-lifecycle-and-chain-of-trust): a serve path that cannot confirm a link is still live past the TTL refuses rather than serving on stale-allowed state.

## Contract Skeleton

The surfaces consuming code needs; the security policies they enforce are fixed by the [Security Contract](#security-contract) above.

```rust
// in capsule-core::sharing
trait ShareLinkIssuer {
    fn create_link(scope: ShareScope, expiry: Option<DateTime>, passphrase: Option<&str>) -> Result<ShareLink, Error>;
    fn revoke(link_id: ShareLinkId) -> Result<(), Error>;
}

// in capsule-api-media::shares
//   GET  /s/{opaque-id}              → metadata blob + LQIP (mandatory server-side strip — see Security Contract)
//   GET  /s/{opaque-id}/blob/{hash}  → ciphertext blob; client decrypts using link-derived key
//   POST /s/{opaque-id}/passphrase   → if passphrase-wrapped, exchange passphrase for unwrap material
```

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
