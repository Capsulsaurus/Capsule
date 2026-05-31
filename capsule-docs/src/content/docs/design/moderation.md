---
title: Moderation
description: Server moderation policy — reports, suspensions, takedowns, blocklists, federated reporting
---

Capsule is end-to-end encrypted, so a server **cannot** scan content it holds — server-side content or CSAM scanning is impossible by design, and no content scanner will be built. Moderation operates entirely on what *is* available: user reports, account-level signals, and federated peer reputation.

Implementation will live in `capsule-api::moderation` (a new sub-crate or service inside `capsule-api`). The boundary surfaces — report submission, federated report exchange, blocklist publication — are the eventual contract; this doc captures what they will need to do.

## What Moderation Cannot Do (Structural)

Naming this up front is load-bearing:

- **No content inspection.** The server holds opaque ciphertext. There is no algorithm that can act on the content of an asset without a key.
- **No retroactive content takedown.** Once a peer has fetched ciphertext, the home server cannot un-fetch it. Takedown is about *future* serving, not deletion-from-everywhere.
- **No silent operations.** Every moderation action that affects user data must produce a [provenance record](/design/cryptography/provenance/#provenance-of-library-modifications) the user can see in their audit log.

## What Moderation Can Do (Operational Hooks)

The actual policy surfaces that need design:

### Federated Reporting

A report against `alice@other.tld`'s asset is routed to her home server's administrators, since they are the only party that can act on her account. Three mechanics are fixed:

- **Authentication.** A federated report MUST be signed by the reporting server's [signing key](/design/federation/#server-identity-and-key-rotation) and is verified before it reaches the admin queue; an unsigned or invalid-signature report is dropped, never surfaced. This makes every report attributable — a server that submits false reports is itself identifiable and blockable.
- **Rate-limiting.** Reports are bounded per `(reporting_server, reported_user)`; exceeding the limit applies backpressure rather than amplifying. Together with signing, this defeats the false-flag / mass-report abuse vector (a flood of forged or spoofed reports against one user).
- **Content.** A report carries the alleged asset's **content hash and album pointer — never plaintext or decryption material**. This is the privacy-preserving, operable middle: the home-server admin can locate the asset and, *if* they already hold album access, fetch and view it to act; an admin without album access sees only opaque identifiers, exactly as the E2EE model requires. A report never widens who can read content.

### Blocklists

Server-level blocklists, plus per-user blocks that federate:

- **Server-level blocklist.** A server admin publishes a list of peer servers that this server refuses to accept federated requests from. Operates at the [federation capability](/design/federation/#federation-capabilities) layer.
- **Per-user block.** A user can block another user; the block is enforced by the blocker's home server — the blocked user is removed from albums shared with the blocker and cannot share new albums with them. Removal is an ordinary MLS `Remove` + AMK epoch bump applied at the blocked user's next sync; the prior epochs' keys they already hold are not retroactively clawed back (consistent with [removal semantics](/design/cryptography/mls/#remove-user-charlie)). A per-user block is **scoped to that user**: it does **not** propagate as a server-wide federation block, so one user (or a coordinated group) cannot weaponize blocks to sever an entire peer server from the federation. Each home server enforces only its own users' blocks.
- **Blocklist exchange (v2).** A peer-level mechanism for sharing *server-level* blocklists across federated servers (so a malicious server isn't pure whack-a-mole) is **deferred to v2**, but its shape is fixed now: signed, versioned blocklist documents an admin **opts into** consuming from peers they already trust — never auto-applied, and deliberately distinct from per-user blocks (which never propagate). v1 ships only the manual server-level blocklist above.

### Untrusted-Server Whitelist

[Federation — Security Against Malicious Files](/design/federation/#security-against-malicious-files) names this as the front-line abuse control for content from servers Capsule does not trust. Moderation policy decides what "trusted" means and how trust is established/revoked.

### Account Suspension

A server admin can suspend a user account on their home server. Suspended accounts:

- Cannot upload — `POST /upload` session creation is refused with a structured `403 AccountSuspended` code (distinct from quota and permission rejections, so the client surfaces the right remediation).
- Cannot share new albums (existing shares remain valid for the share-link TTL; revocation lists can revoke them).
- Cannot revoke other devices' sessions (a suspended account's `revoke_all_sessions` is refused — defends against compromised-account-as-DoS).

The user's *data* is untouched — suspension is an access-level action, not a data-level one. Reversibility (a suspension can be lifted) is the default; permanent termination is a separate policy.

### Takedown

When a moderation action requires the *home server* to stop serving a specific asset (e.g. legal request, CSAM report verified by admin viewing in their album):

- The asset is marked unservable on the home server (`served = false` in the index).
- Federated peers fetching the asset receive `410 Gone`.
- The asset's underlying blob is **not** deleted — the user owns the data, and a takedown is a serving constraint, not a destruction; the user can still restore from their own backup. A takedown is therefore **reversible by default** (an admin can lift it). A **legal-hold** variant marks the asset indefinitely unservable where law requires it — lifted only when the legal obligation ends, not at admin discretion — but even then never destroys the user's bytes: the constraint is on the *home server's serving*, not on the data the user holds.
- The takedown emits a **server-visible moderation provenance record** the user sees in their audit log — what was taken down, when, and (where policy permits) why — honoring the "[No silent operations](#what-moderation-cannot-do-structural)" rule. A user whose asset stops serving is never left to guess why, and the moderation action is itself auditable after the fact.

## Federation Boundary

Moderation crosses the federation boundary cleanly because [federation](/design/federation/) is pull-only and capability-gated. A blocked peer cannot pull; a takedown asset returns `410` to every pull. The moderation policy decisions don't require new federation primitives — they reuse the capability and revocation surfaces already there.

## Appeals

A suspended or taken-down user can appeal. The appeal is authenticated by **master-key proof** (the same mechanism as [global session revoke](/design/authentication/#explicit-revocation)) rather than a session token — the session may be the thing under dispute — and lands in the home-server admin queue. The admin's decision is itself a [moderation provenance record](/design/cryptography/provenance/#provenance-of-library-modifications) the user can see. Because suspension and takedown are reversible by default, a granted appeal simply lifts the constraint.

## Validation

- Federated report transport (smoke): send report from server A to server B; assert it reaches the admin queue with structured metadata.
- Blocklist enforcement (smoke): blacklist a peer; assert federation pulls from that peer are refused.
- Suspension enforcement (unit): a suspended account's upload session creation is rejected with the right structural code.
- Takedown serving (smoke): take down an asset; assert subsequent fetches return `410`; assert the underlying blob is preserved; assert a moderation provenance record is appended and visible in the user's audit log.
- Federated-report authentication (unit): submit a report signed by a valid peer key; assert it reaches the admin queue. Submit an unsigned / invalid-signature report; assert it is dropped. Exceed the per-`(reporting_server, reported_user)` rate limit; assert backpressure.
- Block scoping (unit): a per-user block removes the blocked user from the blocker's shared albums; assert it does **not** appear as a server-level federation block against the blocked user's home server.
