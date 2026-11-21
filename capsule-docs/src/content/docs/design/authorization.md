---
title: Authorization
description: The closed lifecycle-action set, album membership on the server, freshness-gated operations, and the rate limit every endpoint is charged against
audit: none
---

**Rule:** every lifecycle transition is an asset manifest ([](/design/cryptography/provenance/#asset-manifest)) signed under the album's per-epoch write-tier key, the same proof as a write. There is no weaker path to destroy data than to add it. Clients reject anything `verify_asset` ([](/design/cryptography/keys/#write-authorization)) does not accept.

## The Closed Action Set

`action` is a **closed enum**; any other value is a structural error ([](/design/threat-model/schema-rules/#closed-enums)).

| Action | Meaning |
| --- | --- |
| `create` | First write; `prior_provenance_hash` is `null` |
| `replace` | New original bytes (e.g. re-encryption), same `file_id`/`album_id` |
| `delete` | Soft-delete into trash for a retention window ([](/design/organization/#retention-window)) |
| `metadata-update` | Edit the encrypted metadata blob or sidecar fields |
| `derivative-add` | Add a derivative ([](/design/cryptography/provenance/#derivative-provenance)) |
| `derivative-replace` | Replace a derivative; the only path, since silent overwrite is rejected |
| `trash-restore` | Recover from trash within the window |

Adding a value requires a new `protocol_version`; albums stay pinned to their original set.

## Authorizing a Lifecycle Operation

A client accepts an operation only if:

- it is signed with the album's write-tier key for its epoch **and** carries a valid hybrid `device_sig`, both checked in `verify_asset`;
- its `prior_provenance_hash` equals the current chain head (every action but `create`). Stale or forked positions are rejected.

**The chain is the only order.** An asset's operations are totally ordered by their position on its provenance chain. A writer can extend the chain only at its head, and the server enforces that with invariant 17 of [](/design/threat-model/validation/#server-side-validation-invariants). Every merge that needs an order reads chain position, including the LWW registers of [](/design/metadata/#collaborative-metadata). No merge reads a self-asserted `ts` or `timestamp`, which are audit-only ([](/design/threat-model/schema-rules/)). An edit made offline against an old head is refused as a fork and re-signed on the new head, so the later *accepted* write wins. A writer with a clock set to 2099 wins nothing it did not append last.

## The Lifecycle Write Surface

Every non-upload lifecycle write goes through **one** endpoint, `POST /v1/albums/{album_id}/ops`, carrying the signed manifest (plus metadata blob if any); see [](/reference/api/albums/). Writes that move blob bytes (`create`, `replace`, derivatives with new bytes) use the upload protocol ([](/design/transfer/upload/)).

- The structural envelope (invariants 16–18 and 25 of [](/design/threat-model/validation/#server-side-validation-invariants)) runs before any row is written. New actions never add endpoints.
- **Idempotent by content hash:** a replayed accepted manifest returns the identical response and writes nothing.
- Provenance append and per-album `sync_seq` mint are **one transaction**, so an operation is visible on the sync feed ([](/design/transfer/download/)) exactly when durable.
- Rejections carry an `error.*` code ([](/design/i18n/#server-error-codes)), write nothing, and are logged.

## Album Membership on the Server

The server cannot read the MLS group. It holds the owner's **signed roster** (`PUT /v1/albums/{album_id}/roster`, invariant 35 of [](/design/threat-model/validation/#server-side-validation-invariants)): members with role `reader`/`writer`, a strictly increasing `roster_version` and the AMK epoch, signed by a non-revoked owner device. The version may rise at most +16 over the held one, so one publish cannot latch the counter. Removal is a later roster omitting the member, recorded with the version and epoch, not deleted.

- **The roster mirrors MLS, and one author writes both.** Roles are group state: the owner's commits carry the role map in a group-context extension, and adds or removes of other users and every role change are owner-only commits ([](/design/cryptography/keys/#key-delivery-and-who-may-send-it)). Only the owner publishes the roster, so the two can never have different authors. A writer's commits touch only its own account's leaves, which the per-account roster does not list. Members check committers against MLS state, never against the roster.
- **The roster moves first.** The owner's device publishes a membership change to the server before the MLS commit that makes it. An added member is listed before its `Welcome` is sent, so its first fetch is admitted. A removed member is omitted before the `Remove` commit, so its transport is cut no later than its key access. If the commit then fails, the member is cut off while still holding only the keys of epochs it already held, and the device retries the commit.
- **The owner's devices reconcile the two on every sync.** A member omitted from the roster but still in the group gets the missing `Remove`. A listed member whose add was abandoned gets a roster omitting it. The window in which they disagree is one owner-device sync interval.

| Caller | Server behaviour |
| --- | --- |
| Current writer | May upload and post ops; filed under the owner's namespace, billed to the uploader |
| Current member (any role) | May fetch blobs |
| Former member | `403` ([](/design/transfer/download/)) |
| Never on roster | `404`, as for an unknown address |

The roster is a transport control, not confidentiality. Removing a writer reclaims nothing: their uploads stay, still billed to them and deletable only by the owner or a current writer ([](/design/quota/#scope-decisions)).

## Partner Sharing

A **partner grant** shares a user's albums with another user as a standing rule rather than album by album: every album the owner creates from then on is shared with the partner, and the owner may add existing albums in one step.

- **A signed record.** The grant names the partner, the role (`reader` or `writer`) and the scope (future albums, plus optionally a listed set of existing ones). It is signed by an owner device and kept in the owner's library-settings document ([](/design/metadata/#the-library-settings-document)), so the server never learns who is a partner.
- **The owner's devices carry it out.** Adding a member is an owner commit ([](/design/cryptography/mls/#add-user-bob-to-album)) that needs the album's keys, which the server does not hold. On album creation, and on every sync, an owner device adds the partner to each album the grant covers that lacks them. An album created while no owner device is online is shared at the next owner sync; the app says "shared when your devices next sync", never that it is shared already.
- **Revocation** removes the partner from every album the grant added them to, each an ordinary `Remove` that advances the epoch ([](/design/cryptography/mls/#remove-user-charlie)). Albums the owner shared with the partner by hand are left alone.
- **In the partner's timeline.** Whether a partner's albums appear in the partner's own timeline, or only under Shared, is a per-viewer setting in the partner's library settings.
- **Federated.** A partner on another server is added like any federated member ([](/design/federation/)).

## The Server Executes But Never Authorizes

The server carries out deletes and replaces but never permits them. Without keys, it still enforces the structural envelope ([](/design/threat-model/validation/#server-side-validation-invariants)) before persisting.

## Deletes Are Soft First

- `delete` moves the asset to trash; the blob is hard-purged only after the retention window.
- The window is **signed into the delete manifest**, so a server cannot shorten or extend it ([](/design/organization/#recycling)).
- `trash-restore` before expiry is itself an audited manifest.
- **Only the creator's account or the owner shortens a window.** A `delete` on an asset another account created MUST carry at least the album's default retention. A shorter-retention re-issue of it is valid only when signed by a device of the creating account or of the owner. Clients refuse any other such manifest in `verify_asset`, and the server refuses it with `403`, because the server knows both the creating account and the caller.

### What One Hostile Writer Can Do

Any current writer can move every asset in a shared album to trash, including assets other members created. Nothing more is silent. Every such delete is a signed, attributed provenance record, and the trash keeps each asset for the album's whole retention window, during which any member restores it. A writer cannot purge another member's asset early, and cannot make a delete vanish from the chain. The owner's remedy is removing the writer ([](#album-membership-on-the-server)) and restoring from trash. A removed writer's own uploads stay, billed to them, until the owner or a current writer deletes them ([](/design/quota/#scope-decisions)).

## Every Transition Is Auditable

Each operation appends a provenance record ([](/design/cryptography/provenance/#provenance-of-library-modifications)); the chain is append-only ([](/design/threat-model/scenarios/#provenance-immutability-rules)).

## Federated Peers

Federated operations ([](/design/federation/)) pass the same `verify_asset` and envelope checks; peer ordering and timestamps are never trusted. Stale revival ([](/design/sync/#stale-revival-detection)) is caught by the chain check and quarantined.

## Freshness-Gated Operations

A session token alone does not authorize an operation that would let a thief keep the account or destroy its recovery. Such an operation requires a **fresh credential**: the session's `authenticated_at` within **5 minutes**. A refresh carries `authenticated_at` forward and never resets it. Re-authentication resets it, by password ([](/design/authentication/#sign-in-limits)) or through the identity provider ([](/design/authentication/#signing-in-through-an-identity-provider)). A stale session is `403` with a coded refusal that tells the client to re-prove.

| Operation | Gate | What a stolen session could otherwise do |
| --- | --- | --- |
| Issue an enrollment code (`POST /v1/auth/devices/enroll`) | fresh credential | Enroll its own device ([](/design/device-enrollment/#cross-device-add)) |
| Begin TOTP enrollment (`POST /v1/auth/totp/enroll`) | fresh credential | Bind its own authenticator and lock the owner out of new sign-ins |
| Store or replace the master-key escrow or the sealed identity (`PUT /v1/auth/escrow`, `PUT /v1/auth/escrow/identity`) | fresh credential | Overwrite the account's recovery path with junk |
| Disable TOTP (`POST /v1/auth/totp/disable`) | a live code | Switch off the factor |
| Change the password (`POST /v1/auth/password`) | the current password | Take the password door |
| Revoke every session (`POST /v1/auth/logout/all`) | IK proof against the anchor | Sign the owner out everywhere |

Album key escrow slots are not gated, because every push writes them ([](/design/backup-recovery/#album-key-escrow)).

## Rate Limits

Every budget the server enforces is declared once, in `capsule-server/src/counter/budgets.rs`, and stated normatively here. A new route MUST add a row or name the existing row that bounds it.

**How a budget is charged.** Each budget is a fixed window counted atomically: one operation charges and decides together. It is charged **before** the work it protects and on every attempt whatever its outcome, so a caller who guesses right on the last try has still paid for every try. Where a route has a per-source and a per-target budget, the per-source budget is charged first, so one caller cannot spend a bucket every other caller shares. A source is the client address resolved through the deployment's trusted proxies: an IPv4 address, or an IPv6 `/64`, because a single subscriber is routinely handed a whole `/64`. Behind a proxy the deployment MUST name it (`TRUSTED_PROXIES` or `TRUSTED_PROXY_HOPS`), or every client shares the proxy's budget. The server logs a warning when it sees a forwarding header while trusting no hop. A key derived from caller input is built only after the input is validated, or goes to one fixed bucket.

**Store.** *Shared* is PostgreSQL `rate_counters`, exact across replicas. *Process* is an in-memory counter in the process that owns the upload's blob root. It is used only for the two per-chunk throughput budgets, which protect disk and CPU rather than a secret ([](/design/filesystem/server/#required-services)).

| Surface | Operations | Key | Budget | Store |
| --- | --- | --- | --- | --- |
| Registration | `POST /v1/auth/register` | source | 10 / h | shared |
| Password sign-in | `POST /v1/auth/login` | source | 60 / 15 min, shared with re-authentication | shared |
| | | presented address (keyed hash) | soft delay: 5 free failures, then a not-before doubling from 1 s to 15 min; decays 24 h after the last failure; clears on success or a password change | shared |
| | | sign-in device token, replacing the address delay | 10 failures / 24 h | shared |
| Re-authentication, password change | `POST /v1/auth/reauthenticate`, `POST /v1/auth/password` | source | the sign-in source budget | shared |
| | | session | 5 failures / 15 min | shared |
| Second factor | `POST /v1/auth/login/verify-totp`, `POST /v1/auth/totp/verify-enrollment`, `POST /v1/auth/totp/disable` | challenge: the login challenge, the pending enrollment, or the presenting session | 5 / 5 min; a login challenge is single-use | shared |
| | | account, across all three routes | 10 / h | shared |
| TOTP enrollment | `POST /v1/auth/totp/enroll` | account | 10 / h | shared |
| Refresh | `POST /v1/auth/refresh` | lineage | 20 / h | shared |
| Global sign-out | `POST /v1/auth/logout/all/challenge` | account | 10 / h; each redemption burns its challenge, so redemptions are bounded by issuance | shared |
| OIDC begin | `POST /v1/auth/oidc/authorize`, `POST /v1/auth/oidc/device` | source | 10 / min, charged first | shared |
| | authorize | admitted redirect host; refused redirects in one bucket | 60 / min each | shared |
| | device | deployment | 60 / min | shared |
| OIDC completion | `POST /v1/auth/oidc/callback` | source | 30 / min | shared |
| | `POST /v1/auth/oidc/device/poll` | ceremony | one provider call per enforced interval (≥ 5 s) | ceremony record |
| Enrollment issue | `POST /v1/auth/devices/enroll` | account | 10 / h | shared |
| Trusted-contact claim (planned) | claim start | claiming account | 3 / 24 h | shared |
| | | target account | 5 open claims at once; a blocked claim cannot be restarted by the same contact for 7 days | shared |
| Enrollment redemption | `POST /v1/auth/devices/enroll/redeem` | source | 10 / min, charged first | shared |
| | | the 8-digit fallback spelling, deployment-wide | 30 / min, which is the entropy argument for offering it ([](/design/device-enrollment/#cross-device-add)) | shared |
| | | malformed codes, deployment-wide | 60 / min | shared |
| Enrollment relay | relay and drain on `/v1/auth/devices/enroll/channel/{channel_id}` | channel | 120 / min; each mailbox holds at most 16 payloads | shared |
| Escrow fetch | `GET /v1/auth/escrow`, `GET /v1/auth/escrow/identity` | account | 10 / h, each fetch recorded in the activity log | shared |
| Upload creation | `POST /v1/upload` | account, then source | 300 / min, then 600 / min; at most 1,000 live sessions per account | shared |
| Upload chunks | `PATCH /v1/upload/{id}` | account, then source | 3,000 / min, then 6,000 / min | process |
| Deep verify | `POST /v1/storage/verify` with `deep` or `attestation_nonce` | account | 16 GiB re-read / h, up to 100 assets per request; a signed attestation also costs 64 MiB. Every release needs one `deep = intact` per original ([](/design/transfer/durability/#verify-before-destroy)), so the budget is bytes, not requests. A single original larger than the whole window is admitted alone, as the only re-read in its window, so no original is ever unreleasable: it keeps release level with a sustained upload of about 40 Mbit/s, while capping the I/O an account can make the server spend at under 5 MB/s | shared |
| Share serving | `GET /s/{opaque_id}` and its blob and wrapped-secret reads | source, then link | 120 / min, then 60 / min ([](/design/share-links/#security-contract)) | shared |
| | each response that carries bytes, charged its length | source, then link | 4 GiB / h, then 16 GiB / h, both configurable | shared |
| Guest drops | `POST /d/{opaque_id}` | source, then link | 60 / h, then 30 / h ([](/design/web-upload/#security-contract)) | shared |
| Capability mints | `POST /v1/albums/{album_id}/capabilities` | account | 60 / h | shared |
| Federation serving | peer reads of `/v1/sync` and `/v1/blob/{hash}` | peer origin | 10,000 requests and 20 GiB / h; on probation (first 7 days) 1,000 and 1 GiB / h ([](/design/federation/#per-peer-compartmentalization)) | shared |
| Federation intake | `POST /v1/federation/reports` | verified peer; peer × reported account; peer across accounts; nonce | 300 / h; 20 / h; 200 / h; one use ([](/design/moderation/#federated-reporting)) | shared |
| | `POST /v1/federation/hints` | verified peer; nonce | 600 / h; one use ([](/design/federation/#pull-hints)) | shared |
| | request signatures | peer × nonce | one use | shared |
| Federation pull | the egress pull worker | source peer | 10 validation failures / h (3 on probation); 60 CPU-s / h (6 on probation) ([](/design/federation/#egress-pull-worker)) | shared |

Every other route is authenticated and bounded by its own structural caps (body size, live-session and slot limits, quota) rather than by a rate.

### When the Counter Store Fails

**Every shared budget fails closed.** A limiter that an attacker switches off by loading the database is not a limiter, and the guesses these budgets bound (six digits, an eight-digit code, a password) are exactly what an outage must not open up.

- An unreachable store answers `503` with the surface's `error.*.unavailable` code and `Retry-After`, never `500`. A full limiter partition answers `429` with its `*_at_capacity` code, because it is the limiter working rather than a store failing. A client therefore never mistakes an outage for a refusal of its credentials.
- **PostgreSQL is the single point of failure for sign-in, and the counters do not make it one.** The counters live in the same database as accounts, password hashes, sessions and device directories ([](/design/filesystem/server/#required-services)). A database that cannot count also cannot verify a password or read a session, so there is no state in which failing open would admit anyone safely.
- **There is no degraded mode for sign-in or writes, by decision.** During an outage `server-info` and the version endpoint still answer, because neither reads the database. Already-running chunk uploads keep their in-process throughput count but stall at the next database write. Every signed-in client keeps its local library fully usable and queues its writes ([](/design/local-gallery/)). Availability is bought with PostgreSQL high availability (a streaming replica with failover), not with a second store. A restore brings counters and sessions back with everything else.
- **Windows are measured on the server's clock**, never the database's, so a server and its database disagreeing about the hour changes no decision.

## Known Gaps

- The deep-verify budget counts requests, 4 an hour per account (`DEEP_VERIFY`, `capsule-server/src/counter/budgets.rs:148`), not bytes re-read.
- Share serving has no byte budget ([](/design/share-links/#known-gaps)).
- Sign-in still charges a per-account lockout budget ([](/design/authentication/#known-gaps)).
- Partner grants are not implemented; they wait on the library-settings document and on owner devices reconciling album membership.

## Validation

- Per action: correct keys verify; a wrong write-tier key, device or `prior_provenance_hash` is rejected; unknown `action` fails the envelope; a stale chain position is quarantined.
- Invariants 16–18 against real Postgres; full lifecycle E2E in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
- **Chain order (unit):** two registers written from one head converge to the later accepted write whatever their `ts`, and a far-future `ts` wins nothing.
- **Roster (integration):** a roster omitting a writer makes that writer's next fetch `403`; a roster signed by a non-owner device is refused; the client reconciler issues the missing `Remove` for a member the roster omits and the group still holds.
- **Early purge (integration):** a writer's shorter-retention re-issue of another account's asset is `403`; the creator's and the owner's are accepted.
- **Freshness (integration):** each gated operation is `403` on a session older than the window, and accepted after re-authentication.
- **Rate-limit table (unit):** a test pins every `CounterKey` variant to a row of this table and every row to a variant, so a budget cannot exist undocumented or be documented and dead. Each shared budget refuses with `503` when the store is unreachable and `429` when its partition is full.
