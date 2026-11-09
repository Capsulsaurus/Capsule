---
title: Federation
description: How Capsule servers share albums across users on different home servers
status: draft
---

Federation lets an album owned on one Capsule server be shared with users whose accounts live on another. It covers **server-to-server** only; device-to-device sync is [](/design/peering/). It adds two things to the existing read primitives: a **capability token** and **per-peer compartmentalization**. Endpoint shapes are in [](/reference/api/federation/).

## Status

Federation is deferred. Every federation operation answers `404 error.federation.not_configured` unless the deployment sets `FEDERATION_ENABLED=true` **and** `FEDERATION_URL`; `serve` refuses to start with the switch on and no URL. Off, the read primitives honour no capability and `server-info` publishes no `federation_url`. The routes stay in the OpenAPI document either way.

Shipped:

- minting, refresh, revocation, the capability arm of the pull path, and request signatures;
- signed report intake and the server-level blocklist;
- the events/hour and bytes/hour budgets with the serving-side probation tier;
- the operator commands that pin, block, unblock and show a peer (`capsule-server peer …`);
- the [egress pull worker](#egress-pull-worker): the job schedule and lease, hint scheduling and the signed [hint channel](#pull-hints) that feeds it, the error budget and circuit breaker, CPU/hour, the receiving-side probation budgets, local album subscriptions (at most 1,000 per account), the pull itself over the server's HTTP client with re-validation of pulled data, and the worker running in `serve`;
- the per-`(receiving_user, source_peer)` caching budget on what a pull holds, the rejected-hash table a pull records each refused stale revival in, and the breadcrumb index read from what a pull committed.

Open: counting pulled blobs in the receiver's `quota_used`. A peer whose key no operator has pinned can present no capability (`401`) and file no report or hint (`403 error.federation.peer_unknown`).

**Before federation is enabled in a real deployment**, every MUST in this document holds, including re-validation of pulled data at the [boundary](#validation-at-the-boundary) and the [per-peer limits](#per-peer-compartmentalization).

## Threat Model

**A remote server is hostile until proven otherwise.** It may be outdated, buggy, compromised or malicious, and peers may collude. Capsule trusts only cryptography it verifies itself; every other peer claim is unverified input. This extends the [cryptography](/design/cryptography/) posture for Capsule's own server ("trust the server for storage, never for authorization") to servers Capsule does not operate.

## Federation Reuses Existing Primitives

Federation introduces **no new data protocol**. A peer reads the same content-addressed primitives a client uses ([Sync](/design/sync/#discovering-what-changed), [](/design/api-surfaces/#surfaces)):

| Operation | Purpose |
| --- | --- |
| `GET /v1/sync?album_id=…&cursor=…` | Metadata changes after a cursor, for an album the peer holds a capability for. |
| `GET /v1/blob/{hash}` (HTTP `Range`) | An opaque ciphertext blob by content address. |
| `Authorization: Bearer <capability>` | Presents a [federation capability](#federation-capabilities). |
| `X-Capsule-Request-Signature` | Binds that presentation to the peer named in `sub` ([request signatures](#request-signatures)). |

Rejections follow [](/reference/api/conventions/#errors). [Hints](/design/notifications/#vocabulary) and presence ride a separate low-trust channel and never feed validation. Capsule recomputes every blob's content hash on arrival, so a peer cannot lie about what a hash contains.

## Pull-Only Federation

Peers **pull** and never push into Capsule's database. Capsule fetches on its own schedule, through its validation pipeline, and writes only what verifies. The one thing a peer may push is an advisory **hint** ("a new event exists in album A"), after which Capsule fetches and validates on its own terms.

## Album Ownership (v1: Single Home Server)

**Each album has exactly one home server**, the authoritative origin for every blob in it; other servers hold caches, never origins. Only the home server serves an asset's *current* manifest. This rules out split-brain ownership, cross-server delete races and conflicting epoch advances without cross-server consensus. Multi-writer albums across servers are v2 ([open question #1](/design/threat-model/schema-rules/#open-questions)); until then, aggregated albums provide multi-party sharing.

## Federated Shared Albums (Aggregated Albums)

An **aggregated album** is N ordinary single-writer container albums, one per contributor on that contributor's home server, presented by clients as one logical album. Servers never learn a group exists, and every rule above still holds per constituent.

### The Album-Group Assertion

The creator mints a `group_id` (UUIDv7) and shares it in the invite. Each contributor writes an assertion into *their own* album's encrypted collaborative-metadata stream ([operation path](/design/metadata/#how-operations-travel)). This doc owns the schema:

```rust
AlbumGroupAssertion {
  group_id:    UUIDv7,
  group_name:  Lww<String>,
  member_hint: Vec<(album_id, home_server)>,  // advisory discovery only, never trusted
}
```

There is **no shared mutable group object**; the group is the union of per-album assertions.

### Membership and Rendering

- A constituent joins the local aggregate only if the user is a **member** of that album (holds its AMK) **and** it asserts the `group_id`. A stranger's album cannot inject itself, since `member_hint` only says where to ask.
- The aggregate is a computed view ordered by `capture_timestamp`, tiebreak `asset_id`, with nothing stored, no keys and no access-control boundary (like [view albums](/design/organization/#system--smart-albums-views)). The name converges by LWW; the cover is a per-viewer setting in the [library-settings document](/design/metadata/#the-library-settings-document).
- An unreachable origin degrades only its own constituent ([connectivity loss](#robustness-against-connectivity-loss)); nothing is removed.

### Leaving, Revocation, and Moderation

- **Leaving** removes your assertion; unsharing your container also cuts access to past photos.
- **There is no group-level kick.** Each contributor controls only their own constituent.
- **Moderation is per-origin.** Blocking a server or user ([](/design/moderation/)) drops their constituent from your aggregate.

## Federation Capabilities

Sharing an album with `alice@other.tld` gives her server an **album-scoped capability token**: a signed, expiring, revocable EdDSA-JWT reusing the [access-token machinery](/design/authentication/#access-token). "Capability token" always means this JWT, never a [share link](/design/share-links/) or [upload link](/design/web-upload/). It is a **transport control, not a confidentiality control**: it gates who may fetch at all, while confidentiality is [MLS album membership](/design/cryptography/mls/).

### Token Contents

The format is normative; every peer parses it. It is signed with the home server's operational key, classical Ed25519 ([carve-out](/design/cryptography/primitives/#signature-scheme)).

| Claim | Meaning |
| --- | --- |
| `iss` | Issuing home server. |
| `sub` | Peer server identity. |
| `aud` | Album id (`urn:capsule:album:UUID`). Verifiers MUST match `aud` against the album, never against themselves. |
| `scope` | `read` or `read-derivative-only`. Derivative-only refuses any `/blob/{hash}` whose server-visible role is `original`. |
| `iat`, `nbf` | Issued-at; not-before (clock-skew tolerance). |
| `exp` | MUST be ≤ `iat` + **24 h**. |
| `jti` | UUIDv7; the revocation key. |
| `min_protocol_version` | The album's pinned `protocol_version`, which selects the peer's parser. The issuer's supported window comes from `server-info`, never this claim. |

### Token Lifecycle and Chain of Trust

1. **Issuance.** Only the album's owner mints, within a per-account mint budget (`429 error.federation.rate_budget_exceeded`). The peer must be a host name (dot-separated letter-digit-hyphen labels, at most 253 bytes, no scheme, port or path), and the grant's deadline at most 90 days past the token's `iat`; anything else is `400 error.federation.capability_malformed`. The token travels in the share invite to Alice's client, which posts it to `other.tld` as a subscription (`capsule federation subscribe`). `other.tld` records it only once it verifies under the issuer's pinned key and names `other.tld` as its `sub`. It holds at most **1,000** subscriptions per account, live or lapsed: one more is `403 error.federation.subscription_limit` until the member unsubscribes from another, while re-posting an invite for an album already subscribed is a renewal and is never refused.
2. **Verification.** The issuer verifies offline against its own key, with no third-party PKI.
3. **Refresh** is authenticated by the previous token and idempotent on `(peer, jti)`. **A refresh never outlives the grant:** the issuer fixes an absolute deadline at mint on its own record (never a claim), and each successor gets `min(default TTL, deadline − now)`. The default deadline is the first token's `exp`, so grants are single-shot unless the owner asked otherwise. Past the deadline is `403 error.federation.capability_expired`; a member no longer on the roster at the granted epoch is `409 error.federation.member_not_on_roster`.
4. **Revocation** revokes the named token **and every successor refreshed from it** ([ADR 0007](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0007-revoking-a-capability-revokes-every-successor-refreshed-from-it.md)), and an operator revoking an issued `jti` through the list follows the same chain. Revoked `jti`s are published at `/.well-known/capsule/revoked-jti` in the same transaction, and expired entries are pruned, bounding the list to 24 h. Peers cache the list for at most **15 minutes**. **Unavailability fails closed:** past 15 minutes without a fresh list, a verifier rejects any token it cannot confirm. Roster removal cuts the member's grants and blocking a peer cuts all its grants; an epoch bump that keeps the member, or a takedown, cuts none.
5. **Expiry.** A token past `exp` is rejected (`401`).

A grant is bound to the epoch its member was admitted at, so a member removed and re-admitted cannot reuse an old token. The binding holds because a removal always advances the epoch: a roster publish that omits a current member at the held `amk_epoch` is refused with `400 error.album.roster_malformed`, decided under the same lock as the version check, and changes nothing.

### Request Signatures

A capability alone is a bearer token that whoever holds it could present. So every request presenting one also carries a signature by the peer named in its `sub`, made with that peer's operational Ed25519 key, which the issuer verifies against the key its operator [pinned](#server-identity-and-key-rotation) for that peer. A leaked capability is then useless without the peer's key. The format is normative; every peer produces it.

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
| `path` | The operation's path as the issuer routes it (`/v1/sync`, `/v1/blob/{hash}` with the hash filled in, `/v1/federation/capabilities/refresh`), without query or fragment. A path prefix in the issuer's base URL is not part of it: a prefixed deployment strips it before routing, so the issuer verifies the path it routed and the peer signs the operation's path whatever its base URL says. Every path segment of the three operations is unreserved characters, so there is no percent-encoding to agree on. |
| `token_sha256` | The lowercase hex SHA-256 of the presented capability: exactly the token bytes after the `Bearer` scheme and its single space. |
| `signed_at` | `t`, as an integer. |
| `nonce` | `n`, as text. |

The issuer refuses the request `401` when the header is absent or malformed, when `t` is more than **5 minutes** from its own clock either way, when the peer has no pinned key, when the signature does not verify, or when this peer used the nonce within the last **10 minutes** (the span in which a signature can still be fresh). These are structural refusals, uncoded like every other the authenticator makes.

- The nonce is remembered only after the signature verifies, so a third party cannot spend a peer's nonces.
- It is not spent for a blocked peer or one over its events budget. Those get their own coded `403` or `429`, and the request stays refused even if the block lifts or the budget window resets before the route decides, so a signature whose nonce was never remembered is never admitted.
- The replay memory fails closed: a counter store that cannot answer, or is full, is a `401`.

Neither the query string nor any other header is covered. What binds the presentation to the peer is a signature only the peer could have made, used once, within minutes of being made. Rewriting a signed request's query in flight needs a position inside its TLS session, and every query parameter these operations take is bounded by the capability itself (`aud` fixes the album). The reads carry no body and the refresh's body is empty. A session token on the same operations ignores the header.

### What a scope hides, and what it does not

`read-derivative-only` controls which **bytes** are served, not which identifiers a peer learns. The peer still receives every sync entry, including the original's `hash`, `role` and `size`, because the signed manifest it must verify names the original's address anyway. It is refused only at `GET /v1/blob/{hash}`, with `403 error.federation.scope_insufficient`. Hiding an original's existence needs a separate album. A **backup** is outside every scope and answers `404`.

## Validation at the Boundary

Every federated write is checked against the [server-side validation invariants](/design/threat-model/validation/#server-side-validation-invariants), including the [federation-pull checks](/design/threat-model/validation/#on-federation-pull-server-to-server). Federation never loosens a rule. In particular:

- **Strict schema** for the album's pinned [protocol version](/design/versioning/#album-protocol-version-pinning); an unknown `crypto_suite_id` or `sidecar_schema` is rejected.
- **Closed enums** (`action`, `content_type`, `DerivativeManifest.role`, `gps.source`); an unknown value is a structural error.
- **Hard caps** on field size, nesting depth, collection length and per-peer rate.
- **Unknown fields** are rejected at top level; unknown CBOR keys inside a known schema are preserved verbatim, never interpreted.
- **Manifest checks** re-run in full on every pull, including the metadata-blob binding.
- **The parser is a security boundary:** memory-safe Rust decoders, and the federation CBOR path is fuzzed.

## Per-Peer Compartmentalization

Each peer is its own blast radius. Transfer budgets per peer cover events, bytes and CPU per hour, and excess is queued or dropped.

| Budget | Serving side | Receiving side |
| --- | --- | --- |
| Events | 10,000/h (1,000 on probation), charged per capability presentation | — |
| Bytes | 20 GiB/h (1 GiB on probation), charged per blob delivery | — |
| CPU | — | 1 CPU-minute/h (6 CPU-seconds on probation) of pull validation time |
| Errors | — | 10/h (3 on probation), then the circuit breaker opens |
| Storage | — | per-`(receiving_user, source_peer)` caching budget, default 25% of the receiver's hard limit |

- **Serving windows** are fixed and keyed on the peer's origin. Every `GET /v1/blob/{hash}` that carries bytes (`200`, `206`) charges the blob's whole size in one atomic weighted charge before the body is sent. A delivery that does not fit what is left of the window is refused whole, `429 error.federation.rate_budget_exceeded`; a revalidation (`304`) is not charged.
- **Receiving CPU** is the [pull worker's](#egress-pull-worker) validation time per source peer, the one per-peer CPU this server spends that it can meter. It is charged in milliseconds after each pull, at least one for any validation. A pull whose validation would take the open window past its budget spends what is left, so the peer's other jobs read it as spent. A pull abandoned at its deadline charges nothing, since its validation time is lost with it; it is a transport failure, and its back-off bounds how often a peer can cost a deadline's worth. A spent peer's jobs are queued to the window's end, neither failed nor backed off.
- **Storage:** cached blobs are charged to the **receiving user's** [quota](/design/quota/#accounting-model), deduped, against the caching budget. Each subscriber of a pulled album is charged its blobs in full. A page that would take any subscriber past its budget writes nothing and defers the album; it costs the peer no error budget, since the peer did nothing wrong.
- **Error budget and circuit breaker:** malformed input trips exponential back-off (`error.federation.circuit_open`).
- **Probation:** a new peer gets tighter quotas, stricter validation and no hints until it has behaved. A peer is on probation for **7 days** from when this server first recorded it (a pin or a block; a key rotation does not restart it), and a peer with no record is on probation.

## Egress Pull Worker

The receiving half of [pull-only federation](#pull-only-federation): what fetches from a peer, when, and what happens when it fails.

- **Ownership.** A task of `serve`, started only when the deployment federates. It runs a round every **30 seconds**, and a round starts only the jobs that are due. It drives the same orchestration as the SDK's client-side pull (sync pages, then blobs, then validation) over the server's rustls HTTP client, signing each presentation as [request signatures](#request-signatures) require.
- **Jobs and leases.** The unit of work is a **job**: one local album subscription and its source peer. A job is claimed under a lease held in volatile state, so two replicas never pull one job and a replica that dies releases its jobs when the lease lapses. The lease is **10 minutes** and one pull is abandoned as a transport failure after **5**, so a live holder always finishes inside its lease; a holder whose lease lapsed and was claimed by another records nothing. A blocked peer's jobs do not run. Losing the volatile state loses only schedules, since a job with none is due.
- **Schedule.** Every job is pulled every **15 minutes**, jittered ±20%, matching the revocation list's cache bound. A [hint](#pull-only-federation) moves a job's next pull earlier, but never sooner than **1 minute** after its previous pull, so a burst of hints is one pull. A hint moves only a job with no failures, so a failing peer cannot hint its own jobs back to the floor. A peer on [probation](#per-peer-compartmentalization) has its hints ignored.
- **Failure policy.** A failed pull backs off exponentially per job, from 1 minute doubling to a 6-hour cap, with full jitter. A transport failure (unreachable peer, timeout, `5xx`) only backs off. A validation failure also spends the source peer's **error budget**. A spent budget, or one the counter store cannot charge, opens the peer's **circuit breaker** (`error.federation.circuit_open`) for the backoff interval of the failure that spent it, during which none of its jobs runs. An opening never shortens a circuit already open. Nothing that fails validation is written ([soft-fail](#soft-fail-semantics)), and a failure never removes an index entry ([connectivity loss](#robustness-against-connectivity-loss)).
- **Caching-budget deferral is not a failure.** A page refused for a subscriber's [caching budget](/design/quota/#accounting-model) defers the album to its next interval with its failure count untouched and the peer's error budget unspent, and the pages before it stay written.

### Pull Hints

A home server tells a receiving server that an album has news with `POST /v1/federation/hints`. A hint is advice only: it carries no data, never reaches validation, and its only effect is the schedule rule above. It names the job pulling `album_id` from the server that signed it, so a peer can hint only about albums the receiver pulls from that peer. It adds no job, changes nothing about what is pulled or from where, and does not reach past a back-off or an open circuit. The format is normative; every peer produces it.

```json
{ "home_server": "home.example", "recipient_server": "receiver.example", "nonce": "0192f1a8-7c4e-4b6a-9d3e-5f7a8b9c0d1e", "album_id": "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60", "sent_at": "2026-09-02T00:00:00Z", "signature": "<base64>" }
```

`signature` is the home server's operational Ed25519 key over the canonical CBOR of a map holding the other five fields, each whitespace-trimmed and otherwise exactly as sent, plus `context`, the text `capsule-federation-hint-v1`, so the signature cannot be mistaken for any other this key makes. `nonce` is 16 to 64 bytes and never signed twice.

The receiver verifies a hint exactly as it verifies a [report](/design/moderation/#federated-reporting), in order:

1. Structural bounds (`400 error.federation.hint_malformed`).
2. Addressed to this server and signed within **5 minutes** of its clock either way.
3. The peer's operator-pinned key: `403 error.federation.peer_unknown` with none, `403 error.moderation.server_blocked` for a blocked peer.
4. The signature. A hint addressed elsewhere, stale, failing verification, or reusing a nonce this peer used within **10 minutes** is `401 error.federation.hint_unsigned`. Nothing is charged before the signature verifies.
5. **600 hints an hour** per verified peer (`429 error.federation.hint_rate_limited`; a full limiter is `429 error.federation.hint_at_capacity`). Only then is its nonce spent.

A verified, admitted hint is answered `202 { received_at }` whether it moved a job, found it already due, named a job the receiver does not hold, or came from a peer on probation. A held job and one that is not cost the same store work, so neither the answer nor its timing tells them apart: a hint is never a way to learn which albums a server pulls. Only a store that cannot answer differs (`500 error.federation.unavailable`). A refused or lost hint costs at most the album's own interval.

## Stale-Revival Defense

The home server serves only an asset's **current** manifest. A presented manifest whose `prior_provenance_hash` is behind the stored `latest_provenance_hash` is rejected `409` and its hash recorded ([soft-fail](#soft-fail-semantics)); receiving clients apply the same check, which needs no keys. Owner: [Provenance](/design/cryptography/provenance/#provenance-of-library-modifications).

## Soft-Fail Semantics

A federated event that fails validation is not applied, not shown and grants no authority, but its hash is **remembered** so Capsule's view does not silently diverge from peers that accepted it. The table is bounded at a default 100,000 entries (`FEDERATION_REJECTED_HASH_CAPACITY`) with a 90-day TTL (`FEDERATION_REJECTED_HASH_TTL_DAYS`), both configurable, evicting LRU by last reference. A reference is the same hash presented again; reading the table is none. Eviction only forgets that a refusal happened: no blob, entry or head is held through a row, and the refusal itself rests on the asset's superseded heads, not on the table.

## Reconstructing State Without Trusting Peers

Capsule never trusts a peer's result order; state is reconciled from content hashes and [manifest](/design/cryptography/provenance/#asset-manifest) signatures. A manifest's `timestamp` is audit-only. A client may diff album state from the home server against a peer to detect a misbehaving server.

## Robustness Against Connectivity Loss

An unreachable remote asset is shown as unavailable and retried, and its index entry is **never removed**. After a configurable budget (default **30 days** of failed pulls) the album is marked **degraded** in the UI. Recovery re-validates and re-enables it.

## Security Against Malicious Files

- Assets from an **untrusted server** are not loaded without explicit user consent.
- The server never decodes media; clients decode remote-origin bytes in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

## Federated Breadcrumb Index

Search has two tiers: the full local [AI/ML](/design/ai/) index for local and cached content, and a lightweight per-asset breadcrumb (hash, timestamp, author, size, album) for accessible remote albums. A breadcrumb is read from the asset's latest entry a pull committed, so it is only ever built from re-validated input and stores nothing of its own; an asset whose latest entry is a delete or a purged tombstone has none. Reads are paged by asset id, at most 1,000 at a time. Viewing a remote album fetches and **promotes** its assets through the normal indexing pipeline, charged to the receiver's quota. Nothing is pre-indexed wholesale.

## Moderation Hooks

Federation supplies the transport for federated reports and the untrusted-server gate; [](/design/moderation/) owns the policy.

## Server Identity and Key Rotation

- **Two keys**, both at the well-known path. The **operational key** (Ed25519) signs server-to-server traffic and capabilities. The **attestation key** (hybrid Ed25519 + ML-DSA-65) signs [custody receipts](/design/transfer/durability/#custody-receipts) and keeps an **append-only history** so old receipts still verify.
- **Peer keys are pinned on first contact.** A rotation is accepted only after **at least two** independent vantage points (trusted peers or a configured notary) agree **unanimously**, and a failed corroboration is surfaced.
- **Today peer keys are operator-pinned only.** `capsule-server peer pin ORIGIN KEY` takes the `signing_key` field of the peer's `server-info`, read out of band, and replaces any key already pinned. `peer block` refuses the peer at every boundary and revokes and publishes every live grant it holds; `peer unblock` keeps the key and restores no grant.

## Validation

- **Capability verify (unit):** mutate each claim; each mutation is rejected with its code.
- **Request signature (unit and integration):** a missing, malformed, stale, forged, replayed or unpinned signature is each `401`; the signing bytes are pinned to a fixed vector.
- **Revocation fail-closed (unit):** a stale list past 15 minutes rejects unconfirmable tokens.
- **Refresh chain (conformance):** revoking the original `jti` revokes every successor.
- **Pull boundary (unit):** each malformed envelope is refused at its invariant.
- **Budgets and circuit breaker (smoke):** exhaustion gives `429`; malformed floods open the breaker.
- **Caching budget (conformance):** a page past a subscriber's budget on a peer writes nothing; a blob counts once across a subscriber's albums on a peer, each peer is its own budget, and each subscriber of a shared album is held to its own.
- **Weighted charge (conformance):** a charge that overruns an open window is refused whole; racing charges admit exactly what fits.
- **Soft-fail (unit):** the rejected-hash table evicts at its cap.
- **Cross-server pull (E2E):** one case in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
