---
title: Moderation
description: Server moderation policy — reports, suspensions, takedowns, blocklists, federated reporting
status: draft
---

Capsule is end-to-end encrypted, so the server **cannot** scan content, and no server-side content or CSAM scanner will be built. (Opt-in [AI/ML](/design/ai/) runs on the client, over the user's own library.) Moderation works on user reports, account signals and peer reputation. Code: `capsule-server::moderation`.

## What Moderation Cannot Do (Structural)

- **No content inspection.** The server holds only ciphertext.
- **No retroactive takedown.** Ciphertext a peer already fetched cannot be recalled; takedown governs future serving.
- **No silent operations.** Every action affecting user data MUST produce a [provenance record](/design/cryptography/provenance/#provenance-of-library-modifications) visible in the user's audit log.

Moderation *actions* have **no wire surface**: there is no admin authentication model over HTTP, so the admin is the **operator on the host**, authenticated by holding the deployment's database credentials, and actions run through operator commands. The admin reads the federated report queue with `capsule-server moderation reports`. The user reads their own record at `GET /v1/moderation/record`.

## What Moderation Can Do (Operational Hooks)

### Federated Reporting

A report against `alice@other.tld` goes to her home server, the only party that can act on her account. Intake: `POST /v1/federation/reports` (gated with the rest of [federation](/design/federation/#status)).

- **Authentication.** A report MUST be signed by the reporting server's [operational key](/design/federation/#server-identity-and-key-rotation) and verified **before** any budget is charged; unsigned or invalid reports are dropped. Peer keys are operator-pinned; the server never fetches a key on first report (TOFU at intake is rejected).
- **Rate limits.** Per `(reporting_server, reported_user)`, per peer across all accounts (the peer chooses `reported_user`), and per peer on total intake. Every one is charged to the verified peer, so nobody can spend a peer's allowance in its name.
- **Replay.** A report is bound to its recipient, its moment and one use: `recipient_server` must name this server, `reported_at` must be within **5 minutes** of the receiver's clock either way, and the receiver accepts each `nonce` (16 to 64 bytes) once per peer for as long as the report could still be fresh. A replayed nonce is refused before any budget is charged. All three are `401 error.moderation.report_unsigned`; a peer retrying signs again with a fresh nonce.
- **Unknown accounts** get the same answer as accepted reports and are filed nowhere, so intake is not an account-enumeration oracle; the operator log records them.
- **Wire form.** JSON: `reporting_server` (a host name), `recipient_server`, `nonce`, `reported_user`, `asset_hash`, `album_id`, `reason` (optional), `reported_at` (RFC 3339), `signature`. The signature is Ed25519 over the **canonical CBOR of the other eight**, each whitespace-trimmed, `reason` included, and otherwise exactly as sent (no case folding, no re-rendered timestamps). The receiver stores the signed bytes verbatim so the signature can be re-verified.
- **Content.** A report carries a content hash and album pointer, **never plaintext or key material**. An admin without album access sees only identifiers; a report never widens who can read content.
- Accepting a report never changes the reported account's standing.

Intake verifies against the key an operator pinned with `capsule-server peer pin`; a peer with none answers `403 error.federation.peer_unknown`. The queue is read with `capsule-server moderation reports`, oldest first, every peer-supplied reason escaped.

### Blocklists

- **Server-level blocklist.** Peers this server refuses, enforced at the [capability](/design/federation/#federation-capabilities) layer: mint, presentation, refresh and report intake all consult it, and blocking cuts and publishes every live grant the peer holds. The operator writes it with `capsule-server peer block|unblock`.
- **Per-user block.** Enforced by the blocker's home server: the blocked user is removed from albums shared with the blocker (one MLS `Remove` of all their devices, one epoch bump) and cannot share new albums with them. Epochs already held are not clawed back ([removal semantics](/design/cryptography/mls/#remove-user-charlie)). A per-user block MUST NOT propagate as a server-level block, so users cannot weaponize blocks against a whole peer.
- **Blocklist exchange (v2).** Signed, versioned server-level lists an admin **opts into** from trusted peers; never auto-applied, never including per-user blocks.

### Untrusted-Server Whitelist

The [untrusted-server gate](/design/federation/#security-against-malicious-files) is the front-line control for content from unknown servers; moderation policy defines how trust is granted and revoked.

### Account Suspension

A suspended account:

- cannot create upload sessions (`403 error.moderation.account_suspended`, distinct from quota and permission refusals);
- cannot share albums or create share or upload links (existing [share links](/design/share-links/) keep serving until expiry or revocation);
- **can still revoke all sessions**, which is gated by [master-key proof](/design/authentication/#explicit-revocation), not standing.

Data is untouched. Suspension is reversible; termination is a separate policy.

### Takedown

- A takedown is a **serving hold on the asset**, never on a content address: two assets may share a blob, and holding an address would take down someone else's photo.
- Peers fetching a held asset get `410 Gone`. Capability URLs ([share links](/design/share-links/#security-contract), drops) answer `404` because they must not confirm existence; a takedown signals removal of content the peer already knows.
- Bytes are **never deleted**. Takedowns are reversible by an admin; a **legal hold** lifts only when the legal obligation ends.
- [Storage verification](/design/transfer/durability/) reports a held blob as `stored` but **not** `retrievable`, so a client never releases its only local copy on the strength of bytes the server will not serve.
- Every takedown emits a moderation provenance record (what, when, and why where policy permits).

## Federation Boundary

Federation is pull-only and capability-gated, so a blocked peer cannot pull and a held asset answers `410` to every pull. Moderation needs no new federation primitives.

## Appeals

An appeal is authenticated by **master-key proof**, not a session token (the session may be what is disputed), and lands in the admin queue. The decision is a provenance record; a granted appeal lifts the constraint.

## Validation

- **Report intake (unit):** valid signature filed; unsigned or invalid dropped; rate limit applies backpressure; unknown account answered like acceptance.
- **Blocklist (smoke):** a blocked peer's pulls and refreshes are refused.
- **Suspension (unit):** upload session creation refused with the suspension code; session revocation still works.
- **Takedown (smoke):** fetch gives `410`; blob preserved; storage verification reports not retrievable; provenance record visible.
- **Block scoping (unit):** a per-user block removes the user from the blocker's albums and creates no server-level block.
