---
title: Moderation
description: Server moderation policy — reports, suspensions, takedowns, blocklists, federated reporting
audit: none
---

Capsule is end-to-end encrypted, so the server **cannot** scan content, and no server-side content or CSAM scanner will be built; opt-in AI/ML ([](/design/ai/)), including the sensitive-content flag ([](/design/ai/#sensitive-content)), runs on the client, over the user's own library. Moderation works on user reports, account signals and peer reputation. Endpoint shapes are in [](/reference/api/moderation/). Code: `capsule-server::moderation`.

## What Moderation Cannot Do (Structural)

- **No content inspection.** The server holds only ciphertext.
- **No retroactive takedown.** Ciphertext a peer already fetched cannot be recalled; takedown governs future serving.
- **No silent operations.** Every action affecting user data MUST append an entry to the user's **moderation record** ([](#the-moderation-record)), which the user reads at `GET /v1/moderation/record`. The [](/design/activity-log/) is a separate record of the user's own security events, not of operator actions.
- **No authorship.** The server holds no device or write-tier key, so it cannot author an asset provenance record, and a record it signed would fail `verify_asset` on every client ([](/design/cryptography/provenance/#asset-manifest)). A moderation action therefore never touches an asset's chain: it changes what the server serves, and the moderation record says so.

Moderation *actions* have **no wire surface**. There is no admin authentication model over HTTP, so the admin is the **operator on the host**, authenticated by holding the deployment's database credentials, and actions run through operator commands. The admin reads the federated report queue with `capsule-server moderation reports`.

### The Moderation Record

The moderation record is a per-account, append-only, hash-chained log of every operator action on that account and its assets: suspension and its lifting, each serving hold placed or released, each appeal decision.

- **Retention.** Entries (`moderation_events`) are kept for the account's lifetime and go with the account, never on a time window, because the record is the user's client-verifiable evidence of what the operator did ([](/design/filesystem/server/#growth-and-retention)). Operator actions, not user traffic, are what grow it.
- **Entry.** A `moderation-record/v1` map: the account, a per-account `seq` and the hash of the previous entry, the action, the asset ids it covers, the reason where policy permits, and the server's `at`.
- **Signed** under the server attestation key ([](/design/federation/#server-identity-and-key-rotation)), whose custody receipts carry another version and field set, so neither can be read as the other.
- **Verified and kept by the client** like a custody receipt ([](/design/transfer/durability/#custody-receipts)): the client checks the signature under its pinned key and the chain against what it already holds, keeps verified entries as evidence, and surfaces a gap or fork. An effect the client observes with no entry to explain it, such as a held answer for its own asset, is surfaced too. The record cannot stop a dishonest operator from acting silently; it makes an honest operator's actions provable and a silent one visible to the user it affects.

## What Moderation Can Do (Operational Hooks)

### Federated Reporting

A report against `alice@other.tld` goes to her home server, the only party that can act on her account. Intake is `POST /v1/federation/reports`, gated with the rest of federation ([](/design/federation/#status)).

- **Authentication.** A report MUST be signed by the reporting server's operational key ([](/design/federation/#server-identity-and-key-rotation)) and verified **before** any budget is charged; unsigned or invalid reports are dropped. Peer keys are operator-pinned with `capsule-server peer pin`, and a peer with none answers `403 error.federation.peer_unknown`. The server never fetches a key on first report (TOFU at intake is rejected).
- **Rate limits.** Per `(reporting_server, reported_user)`, per peer across all accounts (the peer chooses `reported_user`), and per peer on total intake. Every one is charged to the verified peer, so nobody can spend a peer's allowance in its name. The budgets are in [](/design/authorization/#rate-limits).
- **Replay.** A report is bound to its recipient, its moment and one use. `recipient_server` must name this server, `reported_at` must be within **5 minutes** of the receiver's clock either way, and the receiver accepts each `nonce` (16 to 64 bytes) once per peer for as long as the report could still be fresh. A replayed nonce is refused before any budget is charged. All three refusals are `401 error.moderation.report_unsigned`, and a retrying peer signs again with a fresh nonce.
- **Unknown accounts** get the same answer as accepted reports and are filed nowhere, so intake is not an account-enumeration oracle; the operator log records them.
- **Wire form.** JSON: `reporting_server` (a host name), `recipient_server`, `nonce`, `reported_user`, `asset_hash`, `album_id`, `reason` (optional), `reported_at` (RFC 3339), `signature`. The signature is Ed25519 over the **canonical CBOR of the other eight**, each whitespace-trimmed, `reason` included, and otherwise exactly as sent (no case folding, no re-rendered timestamps). The receiver stores the signed bytes verbatim so the signature can be re-verified.
- **Content.** A report carries a content hash and album pointer, **never plaintext or key material**. An admin without album access sees only identifiers, so a report never widens who can read content.
- **Standing.** Accepting a report never changes the reported account's standing.
- **Queue.** `capsule-server moderation reports` lists oldest first, with every peer-supplied reason escaped.

### Blocklists

- **Server-level blocklist.** Peers this server refuses, enforced at the capability layer ([](/design/federation/#federation-capabilities)). Mint, presentation, refresh and report intake all consult it, and blocking cuts and publishes every live grant the peer holds. The operator writes it with `capsule-server peer block|unblock`.
- **Per-user block.** A user's own decision, carried out in two halves, because the server cannot perform an MLS operation:
  - **Server half, immediate.** The blocker records the block with their home server. From then on the server refuses the blocked account every read of an album the blocker owns (sync pages and blob fetches, as a membership refusal) and every new share or invite between the two. A blocked user on another server loses their capability grants for those albums ([](/design/federation/#token-lifecycle-and-chain-of-trust)). This needs no key and holds while every blocker device is offline.
  - **Client half, cryptographic.** The blocker's admin device commits one MLS `Remove` of all the blocked user's devices from each album the blocker administers and shares with them, one epoch bump per album, the next time it is online. Until that commit the blocked user still holds the current epoch, but the server half already stops them fetching anything new. Epochs already held are not clawed back (removal semantics in [](/design/cryptography/mls/#remove-user-charlie)).
  - An album the blocker belongs to but does not administer is the admin's to change; the blocker can leave it.
  - A per-user block MUST NOT propagate as a server-level block, so users cannot weaponize blocks against a whole peer.
- **Blocklist exchange (v2).** Signed, versioned server-level lists an admin **opts into** from trusted peers, never auto-applied and never including per-user blocks.

### Untrusted-Server Whitelist

The untrusted-server gate ([](/design/federation/#security-against-malicious-files)) is the front-line control for content from unknown servers. Moderation policy defines how trust is granted and revoked.

### Account Suspension

A suspended account:

- cannot create upload sessions (`403 error.moderation.account_suspended`, distinct from quota and permission refusals);
- cannot share albums or create share or upload links (existing share links keep serving until expiry or revocation; see [](/design/share-links/));
- **can still revoke all sessions**, which is gated by master-key proof ([](/design/authentication/#explicit-revocation)), not standing.

Data is untouched. Suspension is reversible; termination is a separate policy.

### Takedown

- A takedown is a **serving hold on the asset**, never on a content address, because two assets may share a blob and holding an address would take down someone else's photo.
- **Every path to the bytes consults the holds of the assets it reaches them through.** A blob is fetched by content address, so a fetch is served only through an asset that references that address, that the caller is entitled to read, and that is not held. A held asset is never a path to its bytes. Another asset referencing the same address (a **sibling**) is served or refused by its own hold. A share link reaches a blob through the issuing account's assets that reference it and applies the same rule.
- **The operator sees the siblings.** Placing a hold lists every sibling of each held blob, with its account and album, and the operator chooses whether to hold them in the same action. A hold never extends to a sibling implicitly.
- Peers fetching a held asset get `410 Gone`, which signals removal of content the peer already knows. Capability URLs (share links per [](/design/share-links/#security-contract), drops) answer `404` because they must not confirm existence.
- Bytes are **never deleted**. An admin can reverse a takedown; a **legal hold** lifts only when the legal obligation ends.
- Storage verification ([](/design/transfer/durability/)) reports a held blob as `stored` but **not** `retrievable`, so a client never releases its only local copy on the strength of bytes the server will not serve.
- Every hold placed or released appends a moderation-record entry ([](#the-moderation-record)) to the owning account's record: what, when, and why where policy permits.

## Federation Boundary

Federation is pull-only and capability-gated, so a blocked peer cannot pull and a held asset answers `410` to every pull. Moderation needs no new federation primitives.

## Appeals

An appeal is authenticated by **master-key proof**, not a session token, since the session may be what is disputed, and lands in the admin queue. The decision is a moderation-record entry ([](#the-moderation-record)), and a granted appeal lifts the constraint.

## Known Gaps

- No production path places a hold or applies a moderation action; the stores exist and only tests call them.
- The share-link serve path consults no hold.
- The moderation record is stored unsigned and unchained.
- The per-user block exists only as a client-side MLS removal with no caller, and the server records no block.

## Validation

- **Report intake (unit):** valid signature filed; unsigned or invalid dropped; rate limit applies backpressure; unknown account answered like acceptance.
- **Blocklist (smoke):** a blocked peer's pulls and refreshes are refused.
- **Suspension (unit):** upload session creation refused with the suspension code; session revocation still works.
- **Takedown (smoke):** fetch gives `410`; blob preserved; storage verification reports not retrievable; the moderation-record entry is visible and verifies.
- **Holds and siblings (unit):** a held asset's blob is refused through the held asset on the member path, the federation path and a share link; a caller entitled through an unheld sibling is served; placing a hold lists every sibling.
- **Moderation record (unit):** entries chain and verify under the attestation key; a dropped or reordered entry is surfaced as a gap or fork.
- **Block scoping (unit):** a recorded per-user block refuses the blocked account's reads of the blocker's albums before any MLS commit, the client half removes the user's devices, and no server-level block is created.
