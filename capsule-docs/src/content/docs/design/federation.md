---
title: Federation
description: How Capsule servers share albums across users on different home servers
status: draft
---

Federation lets an album owned on one Capsule server be shared with users whose accounts live on another. It covers **server-to-server** only; device-to-device sync is [Peering](/design/peering/). It adds two things to the existing read primitives: a **capability token** and **per-peer compartmentalization**.

## Status

Federation is deferred. Every federation operation answers `404 error.federation.not_configured` unless the deployment sets `FEDERATION_ENABLED=true` **and** `FEDERATION_URL` (`serve` refuses to start with the switch on and no URL). Off, the read primitives honour no capability and `server-info` publishes no `federation_url`; the routes stay in the OpenAPI document either way.

Shipped: minting, refresh, revocation, the capability arm of the pull path, request signatures, signed report intake, the server-level blocklist and the events/hour budget. Open: [#476](https://github.com/Capsulsaurus/Capsule/issues/476) — the egress pull worker and everything downstream of it (re-validation of pulled data, the receiving-user quota, the breadcrumb index, the rejected-hash table, hints), the weighted budgets (bytes, CPU, error budget, circuit breaker, probation), and the operator command that pins or blocks a peer. Until that command exists, no peer key is pinned, so every capability presentation is refused `401` for want of a key to verify its request signature, report intake answers `403 error.federation.peer_unknown`, and the blocklist is empty.

**Before federation is enabled in a real deployment**, every MUST in this document holds, including re-validation of pulled data at the [boundary](#validation-at-the-boundary) and the [per-peer limits](#per-peer-compartmentalization).

## Threat Model

**A remote server is hostile until proven otherwise.** It may be outdated, buggy, compromised or malicious, and peers may collude. Capsule trusts only cryptography it verifies itself; every other peer claim is unverified input. This extends the [cryptography](/design/cryptography/) posture for Capsule's own server ("trust the server for storage, never for authorization") to servers Capsule does not operate.

## Federation Reuses Existing Primitives

Federation introduces **no new data protocol**. A peer reads the same content-addressed primitives a client uses ([Sync](/design/sync/#discovering-what-changed), [API surfaces](/design/api-surfaces/#surface--transport-map)):

| Operation | Purpose |
| --- | --- |
| `GET /v1/sync?album_id=…&cursor=…` | Metadata changes after a cursor, for an album the peer holds a capability for. |
| `GET /v1/blob/{hash}` (HTTP `Range`) | An opaque ciphertext blob by content address. |
| `Authorization: Bearer <capability>` | Presents a [federation capability](#federation-capabilities). |
| `X-Capsule-Request-Signature` | Binds that presentation to the peer named in `sub` ([request signatures](#request-signatures)). |

Rejections follow [API Surfaces — Rejection Mapping](/design/api-surfaces/#rejection-mapping). [Hints](/design/notifications/#vocabulary) and presence ride a separate low-trust channel and never feed validation. Capsule recomputes every blob's content hash on arrival, so a peer cannot lie about what a hash contains.

## Pull-Only Federation

Peers **pull**; they never push into Capsule's database. Capsule fetches on its own schedule, through its validation pipeline, and writes only what verifies. The one thing a peer may push is an advisory **hint** ("a new event exists in album A"); Capsule then fetches and validates on its own terms.

## Album Ownership (v1: Single Home Server)

**Each album has exactly one home server**, the authoritative origin for every blob in it; other servers hold caches, never origins. Only the home server serves an asset's *current* manifest. This rules out split-brain ownership, cross-server delete races and conflicting epoch advances without cross-server consensus. Multi-writer albums across servers are v2 ([open question #1](/design/threat-model/schema-rules/#open-questions)); until then, aggregated albums provide multi-party sharing.

## Federated Shared Albums (Aggregated Albums)

An **aggregated album** is N ordinary single-writer container albums, one per contributor on that contributor's home server, presented by clients as one logical album. Servers never learn a group exists; every rule above still holds per constituent.

### The Album-Group Assertion

The creator mints a `group_id` (UUIDv7) and shares it in the invite. Each contributor writes an assertion into *their own* album's encrypted collaborative-metadata stream ([operation path](/design/metadata/#how-operations-travel)); this doc owns the schema:

```rust
AlbumGroupAssertion {
  group_id:    UUIDv7,
  group_name:  Lww<String>,
  member_hint: Vec<(album_id, home_server)>,  // advisory discovery only, never trusted
}
```

There is **no shared mutable group object**; the group is the union of per-album assertions.

### Membership and Rendering

- A constituent joins the local aggregate only if the user is a **member** of that album (holds its AMK) **and** it asserts the `group_id`. A stranger's album cannot inject itself; `member_hint` only says where to ask.
- The aggregate is a computed view: ordered by `capture_timestamp`, tiebreak `asset_id`, nothing stored, no keys, no access-control boundary (like [view albums](/design/organization/#system--smart-albums-views)). The name converges by LWW; the cover is a per-viewer setting in the [library-settings document](/design/metadata/#the-library-settings-document).
- An unreachable origin degrades only its own constituent ([connectivity loss](#robustness-against-connectivity-loss)); nothing is removed.

### Leaving, Revocation, and Moderation

- **Leaving** removes your assertion; unsharing your container also cuts access to past photos.
- **There is no group-level kick.** Each contributor controls only their own constituent.
- **Moderation is per-origin.** Blocking a server or user ([Moderation](/design/moderation/)) drops their constituent from your aggregate.

## Federation Capabilities

Sharing an album with `alice@other.tld` gives her server an **album-scoped capability token**: a signed, expiring, revocable EdDSA-JWT reusing the [access-token machinery](/design/authentication/#access-token). "Capability token" always means this JWT, never a [share link](/design/share-links/) or [upload link](/design/web-upload/). It is a **transport control, not a confidentiality control**: it gates who may fetch at all; confidentiality is [MLS album membership](/design/cryptography/mls/).

### Token Contents

The format is normative; every peer parses it.

| Claim | Meaning |
| --- | --- |
| `iss` | Issuing home server. |
| `sub` | Peer server identity. |
| `aud` | Album id (`urn:capsule:album:UUID`). Verifiers MUST match `aud` against the album, never against themselves. |
| `scope` | `read` or `read-derivative-only`. Derivative-only refuses any `/blob/{hash}` whose server-visible role is `original`. |
| `iat`, `nbf` | Issued-at; not-before (clock-skew tolerance). |
| `exp` | MUST be ≤ `iat` + **24 h**. |
| `jti` | UUIDv7; the revocation key. |
| `min_protocol_version` | The album's pinned `protocol_version`; selects the peer's parser. The issuer's supported window comes from `server-info`, never this claim. |

Signed with the home server's operational key, classical Ed25519 ([carve-out](/design/cryptography/primitives/#signature-scheme)).

### Token Lifecycle and Chain of Trust

1. **Issuance.** Only the album's owner mints (`POST /v1/albums/{album_id}/capabilities`), within a per-account mint budget (`429 error.federation.rate_budget_exceeded`). The peer must be a host name — dot-separated letter-digit-hyphen labels, at most 253 bytes, no scheme, port or path — and the grant's deadline at most 90 days past the token's `iat`; anything else is `400 error.federation.capability_malformed`. The token travels in the share invite to Alice's client, which posts it to `other.tld`.
2. **Verification.** The issuer verifies offline against its own key; no third-party PKI.
3. **Refresh** (`POST /v1/federation/capabilities/refresh`), authenticated by the previous token, idempotent on `(peer, jti)`. **A refresh never outlives the grant:** the issuer fixes an absolute deadline at mint, on its own record (never a claim), and each successor gets `min(default TTL, deadline − now)`. The default deadline is the first token's `exp`, so grants are single-shot unless the owner asked otherwise. Past the deadline: `403 error.federation.capability_expired`. If the member is no longer on the roster at the granted epoch: `409 error.federation.member_not_on_roster`.
4. **Revocation** (`DELETE /v1/albums/{album_id}/capabilities/{jti}`) revokes the named token **and every successor refreshed from it** ([ADR 0007](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0007-revoking-a-capability-revokes-every-successor-refreshed-from-it.md)); an operator revoking an issued `jti` through the list follows the same chain. Revoked `jti`s are published at `/.well-known/capsule/revoked-jti` in the same transaction; expired entries are pruned, bounding the list to 24 h. Peers cache the list for at most **15 minutes**. **Unavailability fails closed:** past 15 minutes without a fresh list, a verifier rejects any token it cannot confirm. Roster removal cuts the member's grants; blocking a peer cuts all its grants; an epoch bump that keeps the member, or a takedown, cuts none.
5. **Expiry.** A token past `exp` is rejected (`401`).

A grant is bound to the epoch its member was admitted at, so a member removed and re-admitted cannot reuse an old token. That binding holds because a removal always advances the epoch: `PUT /v1/albums/{album_id}/roster` refuses a roster that omits a current member at the held `amk_epoch` with `400 error.album.roster_malformed`, decided under the same lock as the version check, and changes nothing.

### Request Signatures

A capability alone is a bearer token: whoever holds it could present it. So every request that presents one also carries a signature by the peer named in its `sub`, made with that peer's operational Ed25519 key, and the issuer verifies it against the key its operator [pinned](#server-identity-and-key-rotation) for that peer. A leaked capability is then useless without the peer's key. The format is normative; every peer produces it.

The header is declared on the three operations a capability is presented to: `GET /v1/sync`, `GET /v1/blob/{hash}` and `POST /v1/federation/capabilities/refresh`. It is a comma-separated list of exactly three `key=value` members, each once, in any order, with optional whitespace around each member:

```text
X-Capsule-Request-Signature: t=1767225600, n=AZQ3hW1ae6qB2vX9k0cR0w, s=<signature>
```

| Member | Meaning |
| --- | --- |
| `t` | When the peer signed, in decimal Unix seconds. |
| `n` | A nonce the peer never signs twice: 22 to 128 characters of the unpadded base64url alphabet, so at least 16 bytes. |
| `s` | The Ed25519 signature, unpadded base64url (64 bytes). |

The signed bytes are the canonical CBOR of this map, the encoding every other signed document in Capsule uses:

| Key | Value |
| --- | --- |
| `context` | The text `capsule-federation-request-v1`, so the signature cannot be mistaken for any other this key makes. |
| `method` | The request method, upper case. |
| `path` | The request path as sent, without the query or fragment. Every path segment of the three operations is unreserved characters, so there is no percent-encoding to agree on. |
| `token_sha256` | The lowercase hex SHA-256 of the presented capability, exactly the token bytes that follow the `Bearer` scheme and its single space. |
| `signed_at` | `t`, as an integer. |
| `nonce` | `n`, as text. |

The issuer refuses the request `401` when the header is absent, is malformed, or has `t` more than **5 minutes** from its own clock either way. It also refuses `401` when the peer has no pinned key, when the signature does not verify, or when the nonce was already used by this peer within the last **10 minutes** (the span in which a signature can still be fresh). These are structural refusals, uncoded like every other the authenticator makes. The nonce is remembered only after the signature verifies, so a third party cannot spend a peer's nonces. It is not spent for a blocked peer or one over its events budget: those are answered by their own coded `403` or `429`. The replay memory fails closed: a counter store that cannot answer, or is full, is a `401`.

Neither the query string nor any other header is covered. A signature only the peer could have made, used once, within minutes of being made, is what binds the presentation to the peer. Rewriting a signed request's query in flight needs a position inside its TLS session, and every query parameter these operations take is bounded by the capability itself (`aud` fixes the album). The reads carry no body, and the refresh's body is empty. A session token on the same operations ignores the header.

### What a scope hides, and what it does not

`read-derivative-only` controls which **bytes** are served, not which identifiers a peer learns. The peer still receives every sync entry, including the original's `hash`, `role` and `size`, because the signed manifest it must verify names the original's address anyway; it is refused only at `GET /v1/blob/{hash}` with `403 error.federation.scope_insufficient`. Hiding an original's existence needs a separate album. A **backup** is outside every scope and answers `404`.

## Validation at the Boundary

Every federated write is checked against [Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants), including the [federation-pull checks](/design/threat-model/validation/#on-federation-pull-server-to-server). Federation never loosens a rule. In particular:

- **Strict schema** for the album's pinned [protocol version](/design/versioning/#album-protocol-version-pinning); unknown `crypto_suite_id` or `sidecar_schema` is rejected.
- **Closed enums** (`action`, `content_type`, `DerivativeManifest.role`, `gps.source`); an unknown value is a structural error.
- **Hard caps** on field size, nesting depth, collection length and per-peer rate.
- **Unknown fields:** top-level rejected; unknown CBOR keys inside a known schema preserved verbatim, never interpreted.
- **Manifest checks** re-run in full on every pull, including the metadata-blob binding.
- **The parser is a security boundary:** memory-safe Rust decoders, and the federation CBOR path is fuzzed.

## Per-Peer Compartmentalization

Each peer is its own blast radius:

- **Transfer budgets** per peer: events, bytes and CPU per hour; excess is queued or dropped.
- **Storage:** cached blobs are charged to the **receiving user's** [quota](/design/quota/#accounting-model), deduped, under a per-`(receiving_user, source_peer)` cap.
- **Error budget and circuit breaker:** malformed input trips exponential back-off (`error.federation.circuit_open`).
- **Probation:** a new peer gets tighter quotas, stricter validation and no hints until it has behaved.

## Stale-Revival Defense

The home server serves only an asset's **current** manifest. A presented manifest whose `prior_provenance_hash` is behind the stored `latest_provenance_hash` is rejected `409` and its hash recorded ([soft-fail](#soft-fail-semantics)); receiving clients apply the same check. The check needs no keys. Owner: [Provenance](/design/cryptography/provenance/#provenance-of-library-modifications).

## Soft-Fail Semantics

A federated event that fails validation is not applied, not shown and grants no authority, but its hash is **remembered** so Capsule's view does not silently diverge from peers that accepted it. The table is bounded: default 100,000 entries, 90-day TTL, LRU by last reference, both configurable.

## Reconstructing State Without Trusting Peers

Capsule never trusts a peer's result order; state is reconciled from content hashes and [manifest](/design/cryptography/provenance/#asset-manifest) signatures. A manifest's `timestamp` is audit-only. A client may diff album state from the home server against a peer to detect a misbehaving server.

## Robustness Against Connectivity Loss

An unreachable remote asset is shown as unavailable and retried; its index entry is **never removed**. After a configurable budget (default **30 days** of failed pulls) the album is marked **degraded** in the UI. Recovery re-validates and re-enables it.

## Security Against Malicious Files

- Assets from an **untrusted server** are not loaded without explicit user consent.
- The server never decodes media; clients decode remote-origin bytes in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

## Federated Breadcrumb Index

Search uses two tiers: the full local [AI/ML](/design/ai/) index for local and cached content, and a lightweight per-asset breadcrumb (hash, timestamp, author, size, album) for accessible remote albums. Viewing a remote album fetches and **promotes** its assets through the normal indexing pipeline, charged to the receiver's quota. Nothing is pre-indexed wholesale.

## Moderation Hooks

Federation supplies the transport for federated reports and the untrusted-server gate; policy is owned by [Moderation](/design/moderation/).

## Server Identity and Key Rotation

- Two keys, both at the well-known path: the **operational key** (Ed25519) signs server-to-server traffic and capabilities; the **attestation key** (hybrid Ed25519 + ML-DSA-65) signs [custody receipts](/design/transfer/durability/#custody-receipts) and keeps an **append-only history** so old receipts still verify.
- Peer keys are pinned on first contact. A rotation is accepted only after **at least two** independent vantage points (trusted peers or a configured notary) agree **unanimously**; a failed corroboration is surfaced. Today peer keys are operator-pinned only.

## Validation

- **Capability verify (unit):** mutate each claim; each mutation is rejected with its code.
- **Request signature (unit and integration):** a missing, malformed, stale, forged, replayed or unpinned signature is each `401`; the signing bytes are pinned to a fixed vector.
- **Revocation fail-closed (unit):** stale list past 15 minutes rejects unconfirmable tokens.
- **Refresh chain (conformance):** revoking the original `jti` revokes every successor.
- **Pull boundary (unit):** each malformed envelope is refused at its invariant.
- **Budgets and circuit breaker (smoke):** exhaustion gives `429`; malformed floods open the breaker.
- **Soft-fail (unit):** the rejected-hash table evicts at its cap.
- **Cross-server pull (E2E):** one case in [Module Map](/design/module-map/#e2e-test-surface).
