---
title: Federation
description: How Capsule servers share albums across users on different home servers
---

Federation lets an album owned on one Capsule server be shared with users whose accounts live on another. This document covers **server-to-server** federation only; direct device-to-device sync for a single user is [Peering](/design/peering/).

Federation reuses the existing read primitives — `/sync`, `/blob/{hash}`, the standard manifest envelope. The only new things federation introduces are a **capability token** (the contract that gates which peers may fetch what) and a **per-peer compartmentalization layer**. Implemented in `capsule-api-sync::federation`: capability issuance, verification, the pull path, and per-peer rate budgeting.

## Threat Model

Federation is designed under one assumption: **a remote server is hostile until proven otherwise.** It may be running ancient, buggy code; it may be compromised; it may be actively malicious; peers may collude. The only thing Capsule trusts is cryptography it verifies itself. Every other claim a peer makes is unverified input until a signature or a content hash says otherwise.

This extends the security posture established in the [cryptography](/design/cryptography/) design toward Capsule's *own* server ("trust the server for storage, never for authorization") to servers Capsule does not even operate.

## Federation Reuses Existing Primitives

Federation deliberately introduces **no new data protocol**. A remote server fetches exactly the same content-addressed primitives a client uses (see [Import — Download & Sync](/design/import/download-sync/#discovering-what-changed)):

| Operation                  | Purpose                                                                                       |
| -------------------------- | --------------------------------------------------------------------------------------------- |
| `GET /sync` (album-scoped) | A page of metadata-blob changes after a cursor, for an album the peer holds a capability for. |
| `GET /blob/{hash}`         | Fetch an opaque ciphertext blob by its content address.                                       |
| `POST` capability proof    | Present a [federation capability](#federation-capabilities) to establish or refresh access.   |

Everything else — notifications, presence — rides a separate, lower-trust channel and never feeds the validation pipeline directly.

Because blobs are content-addressed by their [ciphertext content hash](/design/cryptography/primitives/), a peer *physically cannot* lie about what a hash contains: Capsule recomputes the hash on arrival and rejects a mismatch. This collapses most of the trust problem — Capsule never trusts a peer's *claim* about an object, it fetches and verifies.

ActivityPub and Nextcloud Federated Sharing were considered and rejected as the wire protocol: Capsule's E2EE model (ciphertext-only blobs, MLS-gated album membership) does not map onto either, and adopting one would mean tunnelling Capsule's real primitives through a foreign envelope for no gain.

## Pull-Only Federation

Peers **pull**; they never push into Capsule's database. A remote server fetches on Capsule's schedule, through Capsule's validation pipeline, and the result is written only after it verifies. The single thing a peer may push is a **notification** — "a new event exists in album A" — over the separate low-trust channel; Capsule then fetches and validates on its own terms. Push-based writes are where most federation exploits live, so the design simply does not have them.

## Album Ownership (v1: Single Home Server)

For v1, **each album has exactly one home server** — the server that issued the album's initial capability is the authoritative origin for every blob in it. A peer server that holds cached blobs from a federated album is exactly that: a cache, not an origin. The home server alone serves the *current* manifest for any asset in its album.

This rule keeps the v1 federation API surface small (no replication, no cross-server commit ordering) and forecloses several damage classes — split-brain ownership, two-server delete races, conflicting AMK-epoch advances — that would otherwise need explicit cross-server consensus to prevent.

Cross-server replication of a *single* album (where two users on different home servers each want to write the same album) is **out of scope for v1** and deferred to v2. v1 supports cross-server sharing in the read direction (Alice on `home.tld` shares an album to Bob on `other.tld`; Bob reads via federation; Bob's writes either remain on `home.tld` via a registered or sponsored account, or are out of scope). The v2 design space is flagged in [Threat Model — Open Questions](/design/threat-model/schema-rules/#open-questions).

## Federation Capabilities

Sharing an album with `alice@other.tld` requires her server to be *able* to fetch that album's blobs. Capsule issues her server an **album-scoped capability token**: a signed, expiring, revocable grant naming the album, the scope, and an expiry, reusing the [EdDSA-JWT machinery](/design/authentication/#access-token) already built for access tokens — no separate macaroon or ZCAP format is introduced.

The capability token format is the contract every federated peer parses and that this server signs. Its shape and lifecycle below are normative.

### Token Contents

A federation capability token is an EdDSA-JWT with the following claims:

| Claim                  | Type     | Meaning                                                                                  |
| ---------------------- | -------- | ---------------------------------------------------------------------------------------- |
| `iss`                  | string   | The issuing home server (`home.tld`).                                                    |
| `sub`                  | string   | The peer server identity (`other.tld`).                                                  |
| `aud`                  | string   | The album id this capability scopes to (`urn:capsule:album:UUID`).                       |
| `scope`                | enum     | `read` (full) or `read-derivative-only` (thumbnails and previews only, never originals). |
| `exp`                  | RFC 3339 | Expiry; never more than **24 h** after `iat`.                                            |
| `nbf`                  | RFC 3339 | Not-before; clock-skew tolerance against the peer's wall-clock.                          |
| `jti`                  | UUIDv7   | Unique token identifier; the revocation key.                                             |
| `min_protocol_version` | string   | Lowest `protocol_version` the issuing server still serves; matches the album's pin.      |

Signed under the home server's signing key — classical Ed25519 only, per the [operational-signature carve-out](/design/cryptography/primitives/#signature-scheme).

### Token Lifecycle and Chain of Trust

1. **Issuance.** A user on `home.tld` shares an album with `alice@other.tld`. `home.tld` mints a capability token for `other.tld` and delivers it as part of the share-invite message to Alice's client. Alice's client posts the token to `other.tld`; `other.tld` caches it server-side and uses it on every subsequent pull.
2. **Verification.** Capsule (the verifier, `home.tld` in this case) verifies the token offline against its own published signing key — no third-party PKI, no network call to a notary except for key rotation (see [Server Identity and Key Rotation](#server-identity-and-key-rotation)).
3. **Refresh.** A token nearing `exp` is replaced by `other.tld` requesting a new one on Alice's behalf; the request is itself authenticated by the previous token. Idempotency keyed by `(peer_id, jti)` per [Threat Model — Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants).
4. **Revocation.** Revocation is a short TTL (`exp ≤ 24h`) plus a published **revocation list** at `/.well-known/capsule/revoked-jti`. Peers fetch and cache the list with a **maximum staleness of 15 minutes**. A peer holding a revoked-but-not-yet-expired token will still be honored for up to 15 minutes after revocation — this is the deliberate trade-off between revocation latency and revocation-list polling overhead. **List unavailability fails closed:** a verifier that relies on a *cached* copy of an issuer's revocation list and cannot refresh it must reject, past the 15-minute bound, any token whose `jti` it can no longer confirm against a current list — it never honors tokens indefinitely on a stale list. The `exp ≤ 24h` ceiling caps the worst case regardless, but the explicit rule means revocation cannot be outlived by making the list unreachable. (A server verifying its *own* tokens checks its own always-fresh list and is never stale.)
5. **Expiry.** A token past `exp` is rejected unconditionally; the verifier returns `401` and the peer must obtain a fresh token before continuing.

This capability is a **transport-scoped control, not a confidentiality control**: it gates *who may fetch at all* (rate-limiting, anti-enumeration, clean revocation of a sharing relationship), nothing more. Confidentiality is already enforced by [MLS album membership](/design/cryptography/mls/) — without the album master key, fetched bytes are unreadable.

## Validation at the Boundary

Every byte from a peer crosses a hard boundary before it is trusted. The exhaustive checklist — refuse-by-default, applied to every federated write — is owned by [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants); the rules that follow are the federation-specific specialization of that list.

- **Strict schema match.** Input must conform exactly to the schema for its declared protocol version (see [album version pinning](/design/versioning/#album-protocol-version-pinning)). Anything else is rejected. `crypto_suite_id` and `sidecar_schema` must each be values the verifying server recognizes; an unknown value is **not** preserved-and-ignored, it is rejected (cf. the asymmetric Postel's Law in [Principles](/design/principles/) and [Threat Model — Schema Rules](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).
- **Closed enums.** `action`, `content_type`, `DerivativeManifest.role`, and `gps.source` are closed per protocol version. An unknown value is a structural error, not a "future to ignore."
- **Hard caps.** Size caps on every field, depth caps on nested structures, length caps on bounded collections (e.g. `superseded_captions ≤ 16`), rate caps per peer. No unbounded input reaches a parser.
- **Unknown fields within a known schema preserved, never executed.** Top-level unknown fields are rejected; field-level unknown CBOR keys within a known schema are preserved verbatim for forward compatibility but are never interpreted.
- **Manifest envelope checks.** All items 1–18 of [Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants) apply — `protocol_version` in range, `crypto_suite_id` in inventory, hash length matches the suite's digest size, declared size against received bytes, `created_by_device` in the user's device directory, `timestamp` within the sanity bound, monotonic `amk_version`, and the [stale-revival check](/design/import/download-sync/#stale-revival-detection) on `prior_provenance_hash`.
- **Capability token.** Items 19–21 of the same list: token verifies under the home server's signing key, `exp` in future, `jti` not in the revocation list, per-peer rate budgets unbroken.
- **The parser is a security boundary.** Capsule's decoders for federated input are written in memory-safe Rust against audited libraries (`ciborium`, `serde_cbor`); we explicitly assume the host language and decoder are memory-safe (the same assumption [Security Against Malicious Files](#security-against-malicious-files) makes at the client edge). Decoder CVEs in client decode paths for *opaque media bytes* are handled by the [sandboxed decoder](/design/clients/#sandboxed-decoder), not by re-implementing the decoder. The federation CBOR decode path is additionally fuzzed.

## Per-Peer Compartmentalization

Each peer is its own blast-radius boundary — a bad peer cannot starve good ones:

- **Quotas.** Per-peer budgets (deployment-tuned) on events/hour, bytes/hour, and CPU/hour. Exceeding a budget queues or drops further requests.
- **Receiving-user storage budget.** The per-peer budgets above bound *transfer*; storage is bounded separately. Blobs a pull *caches* on the home server are charged to the **receiving user's** [quota](/design/quota/#accounting-model), deduped, under a per-`(receiving_user, source_peer)` cap — so a single user pulling from many peers cannot exhaust home storage even while staying within every individual peer's transfer budget.
- **Error budget + circuit breaker.** Malformed input spends a per-peer error budget; enough failures trip a circuit breaker that backs the peer off exponentially (e.g. 5 / 30 / 60 minutes). A buggy peer cannot DoS Capsule.
- **Quarantine for new peers.** First contact puts a server in a probationary tier: tighter quotas, stricter validation, no push notifications accepted. It graduates after a period of clean behavior. This cuts off the "spin up a fresh instance to attack" vector, mirroring email reputation systems.

## Stale-Revival Defense

A federated peer may have cached an old manifest for an asset that the home server has since marked deleted (or otherwise advanced beyond). Submitting that old manifest back must not silently resurrect the asset. The defense is owned by [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications) and surfaced for federation here:

- The home server only serves the **current** manifest for any asset — it does not expose an API to fetch an arbitrary past manifest. A peer can therefore only present a manifest it has previously cached.
- A peer presenting a manifest whose `prior_provenance_hash` is behind the home server's stored `latest_provenance_hash` is rejected with `409` (stale-revival), and the rejected manifest's hash is added to the bounded rejected-hash table (see [Soft-Fail Semantics](#soft-fail-semantics)). The same defense runs on the receiving client when a peer's pull serves a stale manifest forward.
- The chain check is fully no-key: the server reads `prior_provenance_hash` from the manifest envelope and compares it to its own stored value.

This is the federation-layer specialization of [Threat Model — Damage Scenario Map](/design/threat-model/scenarios/#damage-scenario--invariant-map), row #4.

## Soft-Fail Semantics

A federated event that fails validation is rejected **locally** — not applied, not shown, no authority derived from it — but its hash is **remembered**. Remembering the hash keeps Capsule's view from silently diverging from peers that (wrongly) accepted it: divergence is the real enemy, and explicit rejection-with-memory is the cure. This is the federation-facing counterpart of the [`verify_asset` quarantine](/design/cryptography/keys/#write-authorization) — a failure is never silently dropped and never silently accepted.

**Bounded memory.** A hostile peer could otherwise flood the rejected-hash table indefinitely, so the table is **capped**: default 100,000 entries with a 90-day TTL per entry, both deployment-configurable. Eviction is LRU by last reference within the cap: the hashes that age out are the ones Capsule hasn't seen referenced again, so by the time they age out they are no longer load-bearing for divergence detection.

## Reconstructing State Without Trusting Peers

Capsule never trusts the *order* in which a peer returns results. Federated state is reconciled from cryptographic signals — content hashes and signatures on [asset manifests](/design/cryptography/provenance/#asset-manifest) — not from peer-supplied ordering. A manifest's `timestamp` is self-asserted and used for audit only.

**Cross-peer consistency checks.** As a cheap backstop, a client may periodically fetch the same album state from the home server and from a peer and diff them. A mismatch flags a potentially misbehaving server. This is rare and off the hot path, but one server cannot rewrite history without another noticing.

## Robustness Against Connectivity Loss

Assets linked from external servers may be unreachable — server downtime, network issues. Capsule indicates the asset is currently unavailable and retries fetching it later. It does **not** thrash and remove the external asset's metadata from the local index; the unavailability is logged for debugging and monitoring.

Under the v1 [single-home-server rule](#album-ownership-v1-single-home-server), an unreachable home server makes its assets unreachable but does not produce conflicting state — there is no second authoritative server to diverge from. After a configurable downtime budget (**default 30 days of failed pulls**), Capsule marks the album **degraded** in the UI ("Owned by an unreachable server"). The local index entries are **never removed** — the assets are unreachable, not deleted, and resuming federation with the home server (when it recovers) re-validates and re-enables the album. There is no "kick the server" mechanism in v1 because there is nothing to kick: a single home server's silence is observed as unavailability, full stop.

## Security Against Malicious Files

Linking assets from an external server means a client inherently trusts bytes from that server. Two defenses apply:

- **Untrusted-server whitelist.** If an album contains assets from an untrusted external server, clients skip loading them unless the user explicitly consents, accepting the risk.
- **Sandboxed decoding on the client.** The Capsule *server* never decodes media — it handles only ciphertext — so image-decoder CVEs (libjpeg, libwebp, libheif, libavif have all shipped exploits recently) are a **client-side** risk. Clients decode untrusted remote-origin assets through an isolated/sandboxed decode path that can be crashed freely (see [Clients](/design/clients/)).

## Federated Breadcrumb Index

Search spanning federated albums uses a two-tier index:

- **Tier 1 — local full-fidelity index.** Everything on the home server — own uploads plus cached remote content — gets the full treatment described in [AI/ML Integrations](/design/ai/): embeddings, tags, perceptual hashes.
- **Tier 2 — federated breadcrumb index.** For accessible remote albums, Capsule keeps only a lightweight record per asset — content hash, timestamp, author, size, album membership. When the user actually views the remote album, relevant assets are fetched and **promoted** to full Tier-1 treatment: the [AI/ML](/design/ai/) pipeline indexes them (embeddings, tags), and the fetched bytes count toward the receiver's [quota](/design/quota/#accounting-model). Promotion runs the indexing pipeline rather than copying pre-computed remote state — Tier 2 holds none — and is lazy and on-demand; Capsule never pre-indexes every federated album wholesale.

## Moderation Hooks

Federation introduces moderation hooks for handling abuse across servers; the full policy (reports, suspensions, takedowns, blocklists) is owned by [Moderation](/design/moderation/). Federation provides the transport (federated reports between servers) and the boundary (untrusted-server whitelist that gates content from unknown peers).

## Server Identity and Key Rotation

- Server-to-server requests are signed under the server's signing key (classical Ed25519 only, per the [operational-signature carve-out](/design/cryptography/primitives/#signature-scheme)), published at a well-known path. Matrix, ActivityPub (HTTP Signatures), and AT Protocol all converge on this pattern.
- Servers cache each other's public keys (TOFU-pinned on first contact). A rotation is confirmed by a **perspective check**: before accepting a rotated key, a peer corroborates it against one or more independent vantage points (other servers, or a configured notary) and accepts only on agreement — so a single compromised network path cannot substitute a forged key. A rotation that fails corroboration is surfaced, not silently accepted. This is the mechanism behind [Threat Model — scenario #26](/design/threat-model/scenarios/#damage-scenario--invariant-map).
- Album protocol versions are pinned per album — see [Album Protocol Version Pinning](/design/versioning/#album-protocol-version-pinning).

## Validation

- **Capability token verify (unit).** Generate token; verify under issuer key; mutate each claim; assert each mutation rejected with the right structural code (expired, revoked, wrong audience, wrong scope, missing claim).
- **Revocation-list fail-closed (unit).** Cache a revocation list, then make refresh fail; advance past the 15-minute staleness bound; assert a token whose `jti` cannot be freshly confirmed is rejected, not honored on the stale list.
- **Pull boundary checks (unit).** Submit a peer-pull request with each kind of malformed envelope; assert refusal at the corresponding [server-side invariant](/design/threat-model/validation/#server-side-validation-invariants).
- **Rate-budget enforcement (smoke).** Exhaust a peer's events-per-hour budget against a testcontainer Postgres; assert `429`; assert successful pulls resume after the window.
- **Circuit breaker (smoke).** Submit N malformed payloads from a single peer; assert circuit opens; assert further requests are short-circuited until the back-off elapses.
- **Soft-fail bounded memory (unit).** Push the rejected-hash table past its cap; assert LRU eviction; assert no unbounded memory growth.
- **Cross-peer consistency (smoke).** Stand up two federated servers; produce a write on one; fetch from the other; assert byte-identical state.

The cross-module case — full cross-server pull where Alice on `home.tld` shares to Bob on `other.tld` and Bob's client successfully fetches and verifies — is one bounded E2E case in [Module Map](/design/module-map/#e2e-test-surface).
