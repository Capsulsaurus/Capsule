---
title: Account Activity Log
description: The durable record of an account's security events, what it may hold under E2EE, and who reads it
status: draft
---

The activity log is the account owner's record of what was done **to and with the account** on this server: sign-ins, sign-outs, revocations, credential changes, device enrollment and the links that open the account to someone else. It answers "was that me?" next to the [session list](/design/authentication/#explicit-revocation), and it is what an operator and a user reconstruct an incident from. Code: `capsule-server::activity`.

It is not the [provenance](/design/cryptography/provenance/) chain, which signs changes to *library data*, and it is not the [moderation record](/design/moderation/), which records what an operator did to the account. The three are separate records with separate writers.

## Invariants

- **Owner-only.** An account reads its own log and nobody else's, over a route with no account parameter. There is no admin read over HTTP, for the reason [moderation](/design/moderation/) has no admin surface: no admin authentication model exists. The operator holding the database credentials can read the table, as they can read every table.
- **Append-only.** An event is never updated. Rows leave the log only by the retention bound below, never by a user or a request.
- **Identifiers, never content.** The server is end-to-end encrypted, so no field may carry what only clients can read: no asset content, no album names, no captions, no labels, no file names, and no key material. An event names *which* session, share link or drop link, by the identifier the server already holds.
- **Recording never refuses the act.** An event is written after the action it describes has happened. A store that cannot record it is logged at `error` with the event's fields and the action stands: refusing a sign-out, a revocation or a password change because the log is down would turn an audit outage into a security outage.
- **The event set is closed per server version.** Every event the server writes is a kind listed below. A new kind is an additive change: a client renders a kind it does not know as a generic entry with its time, and never refuses the listing.

## Events

| Kind | Written when | `subject` |
| --- | --- | --- |
| `account.registered` | an account is created, by registration or by the first identity-provider sign-in of an identity | its first session, when registration opened one; absent for an account an identity-provider sign-in created, whose `sign_in.oidc` names the session |
| `sign_in.password` | a password sign-in opens a session | the new session |
| `sign_in.second_factor` | a sign-in completes its second factor and opens a session | the new session |
| `sign_in.second_factor_refused` | a second-factor code is refused for a sign-in whose first factor verified | — |
| `sign_in.oidc` | an identity-provider sign-in (browser or device flow) opens a session | the new session |
| `sign_out` | a session signs itself out | the closed session |
| `session.revoked` | a session revokes another session of the account | the revoked session |
| `sessions.revoked_all` | every session is revoked with master-key proof | — |
| `password.changed` | the account password changes | — |
| `totp.enabled` | a second factor is confirmed | — |
| `totp.disabled` | a second factor is removed | — |
| `escrow.replaced` | the master-key [escrow](/design/backup-recovery/#master-key-escrow) is stored over an earlier one, so the old recovery secret stops working | — |
| `device.enrolled` | a cross-device enrollment code is redeemed | — |
| `share_link.created` | a share link is created | the link's id |
| `share_link.revoked` | a share link is revoked | the link's id |
| `drop_link.created` | a guest-upload link is provisioned | the link's id |
| `drop_link.revoked` | a guest-upload link is revoked | the link's id |

Not recorded, each for a stated reason:

- **Password sign-in failures.** The account directory answers a refused or locked sign-in without naming the account, so that the route cannot become an enumeration oracle; recording one needs the directory to name the account it refused, which is a change to that port.
- **Blob downloads.** One event per blob per request would grow with library size and record an access pattern over content addresses. Downloads by a share link or a federated peer are the security-relevant subset; they need an aggregation (per link or peer, per window) before they are recorded.
- **A first escrow, and the sealed identity and album-key slots beside it.** A first escrow invalidates no recovery secret, and the other slots are sealed under the master key, so replacing them changes nothing that opens the account. Only replacing the master-key escrow is a credential change.
- **Token refresh.** It rotates a session that is already recorded, once per access-token lifetime, and would bury every other event.

## Fields

| Field | Meaning |
| --- | --- |
| `event_id` | A UUIDv7 the server mints for the event |
| `kind` | One of the kinds above |
| `at` | The server's clock when the event was recorded |
| `request_id` | The `X-Request-Id` of the request that caused it ([request ids](/design/api-surfaces/#negotiation-across-transports)); absent for an event no request caused |
| `session_id` | The session that acted, when one did; absent for an unauthenticated act such as a sign-in or a revoke-all |
| `subject` | The identifier the event is about, per the table above |

No client address and no user agent. An address is the most useful field for spotting a stranger's sign-in and the most sensitive one to keep: it is personal data, it outlives the session it describes, and it is the one field that cannot be withdrawn from a log once written. Adding it later is additive; removing it is not.

The `request_id` ties an event to the server's own log lines for the same request, which carry the method, route, client address and outcome for as long as the operator's log retention keeps them. The activity log is the durable half, the request log the detailed half.

## Retention

An account's log keeps events for **90 days** and at most **1,000 events**, whichever bound is reached first:

- A read never returns an event older than 90 days, whether or not it has been deleted yet.
- Recording an event deletes the same account's events beyond either bound, in the same operation, so the log of an active account never exceeds its bound.
- An account that records nothing more keeps its expired rows, unread, until it next records. Deleting them sooner is an operator sweep over the table, not a request's work.
- The count bound is what keeps a burst of events (a scripted sign-in loop against the second factor) from growing the table without limit; the rate limits below already slow it.

## The Port

The log is a durable port in the PostgreSQL family, beside the asset index, accounts and moderation, with an in-memory adapter for tests and `--memory` deployments. Both adapters pass one conformance suite. It is not a generic event store: it records the closed set above and reads one account's events, newest first.

## Reading It

`GET /v1/auth/activity` returns the caller's own events, newest first, at most `limit` of them (default 100, maximum 1,000, which is the retention bound). The CLI reads it with `capsule auth activity --json`.

## Rate Limits

No event feeds a [counter budget](/design/authentication/#the-second-factor). Budgets are charged at the request edge, before the action and against a counter store built for that; the log is written after the action, to PostgreSQL. A budget fed from the log would make a log outage disable a limiter. The events that matter for abuse — sign-ins and second-factor attempts, link creation — are already budgeted at the routes that cause them.
