---
title: Authentication
description: Identity, account portability, session and access tokens
status: draft
---

The server can prove a request comes from a session it issued, not who the user is. The **master key**, held client-side, is the identity root. Every login credential authenticates a *session* only, and the master key never derives from, and is never visible to, the credential verifier. Endpoint shapes and status codes are in [](/reference/api/auth/).

## Account Types

- **Registered accounts** have a unique identity and their own master key, and sign in with password (+ optional TOTP) or OIDC ([](#choosing-an-auth-path)).
- **Delegated/sponsored accounts** have keys derived from a registered account's master key and no identity of their own. The sponsor authenticates and manages them and has full access ([](/design/cryptography/keys/#delegatedsponsored-accounts)).
- **Non-registered accounts** have no identity or master key: share links ([](/design/share-links/)) encapsulate keys to the link secret, and web-upload links ([](/design/web-upload/)) let a guest seal to a link-scoped key with no read access.

## Creating an Account

Registration takes **an address and a password, nothing else**, and signs the caller in with a token pair.

- No display name, username or invitation code: the server stores as little about a person as it can.
- Passwords have a 12-character floor and no composition rule, because they guard a session, not the master key.
- A taken address is `409 error.auth.user_already_exists`, an account oracle by decision, bounded per source. Every attempt whose body parses, refused ones included, is charged 10/hour against the resolved client address (IPv6 per /64, through `TRUSTED_PROXIES`/`TRUSTED_PROXY_HOPS`). A spent budget is `429 error.auth.rate_limited` and a full limiter partition is `429 error.auth.registration_at_capacity`, both with `Retry-After`. The limiter fails closed: an unreachable counter store is `500`, never an admission. Behind an unconfigured proxy every registrant shares the proxy's budget, as with the share and drop source limiters.
- A new account has **no device directory and therefore cannot write**. Invariant 7's floor is the writing device's `added_at` in the published directory, with no account-creation fallback, so the client's first act is publishing a directory.
- There is no token introspection, since the session ledger is read on every request and validation *is* the request. There is no password reset, since the server cannot re-wrap a master key it never saw; recovery is the escrow blob ([](/design/backup-recovery/)).

### Passkeys Are Not in v1

No passkey surface ships. Building one is new work and reopens the `rustls`-only rule ([](/design/networking/#transport-security)), because `webauthn-rs` links `openssl` for attestation parsing.

### The Second Factor

TOTP is the local path's second factor.

- **Login with a confirmed factor answers `202 Accepted` with a challenge.** No session, cohort or refresh token exists until it completes. Clients read the `202` from the status, and the SDK returns a `LoginOutcome`, never a challenge shaped like a failure.
- **Verifying the challenge opens the session**, so the advisory `cohort_hash`/`device_id` ride that request. The budget is 5 attempts per **challenge**, not per account, because a per-account budget lets anyone lock an owner out.
- **Enrollment gates nothing until confirmed.** It issues a secret and an `otpauth://` URI, is refused over a confirmed factor and replaces a pending one. The confirming code is **spent** into the replay ledger.
- **Disabling requires a live code**, not just a session, so a stolen access token cannot switch off the control.
- **A code is accepted at most once** (RFC 6238 §5.2) by compare-and-set on the highest step used. Parameters are fixed: SHA-1, 6 digits, 30 s step, ±1 step drift.
- **A store outage fails sign-in closed.**
- Reauthentication takes a password alone, because an existing session has already passed the factor.

### The Profile Surface

- **The profile is the caller's own** id, address, optional display name and creation time: the whole of what the server stores about a person. The route has no `{user_id}` segment, so reading another's profile is unrepresentable; other accounts' public facts are the device directory ([](/design/cryptography/keys/#device-directory)).
- **Only the display name is editable.** An absent key leaves it unchanged and an explicit `null` clears it. It is trimmed and ≤128 chars, and control characters are **refused**, not rewritten.
- **A password change authenticates with the current password** through the sign-in path, so lockout applies. It closes **every** session of the account, then re-opens the caller's. A wrong current password is `403`, never `401`.
- **The login address is immutable.** Changing it without proof of control of the new address is an account takeover, so the port has no method for it until such proof exists.

## Choosing an Auth Path

Both paths mint the same sessions ([](#session-and-access-tokens)) and differ only in who verifies the credential. A deployment may enable either or both.

- **Local auth** (password + optional TOTP) is the default, with no external dependency, for personal and household servers.
- **OIDC** (authorization code + PKCE, Capsule as relying party only) suits deployments with an IdP or wanting SSO. Account lifecycle policy lives at the IdP.

### Signing In Through an Identity Provider

Authorization code + PKCE, plus the device grant ([](#signing-in-without-a-browser)). No implicit or hybrid flow.

**Ceremony.** Authorize takes the client's `redirect_uri` and `binding_challenge` and answers the provider URL, `state` and a 10-minute deadline; the client receives the redirect itself. The callback takes `state`, `code` and `binding_verifier` and answers exactly as password login does (token pair or `202` TOTP challenge), through the same session code.

- **Redirect allow-list.** Exactly `OIDC_REDIRECT_URL`, or, only when `OIDC_ALLOW_LOOPBACK_REDIRECT` is on (default off), `http` on `127.0.0.1`/`[::1]` at any port (RFC 8252 §7.3; `localhost` refused per §8.3). The redirect is stored with the ceremony and replayed byte-for-byte to the token endpoint. Refusal is `400 error.auth.oidc_redirect_invalid`.
- **Begin is rate-bounded** as an unauthenticated write: 60/min per admitted redirect host (`429 error.auth.rate_limited`) and the ceremony store's capacity (`503 error.auth.oidc_at_capacity`). The limiter key is chosen **after** redirect validation and all refusals share one deployment-wide bucket, so a caller cannot mint counter keys. Limiter ceilings are partitioned per key kind so a flood on one surface cannot starve another.
- **`state` is single-use**, burned on the first callback whatever the outcome. Unknown, spent and expired are one answer, `401 error.auth.oidc_state_invalid`.
- **Client binding defends against login CSRF.** `binding_challenge` is the S256 hash (43-char unpadded base64url) of a ≥256-bit client secret; a malformed one is `400 error.request.malformed` before any budget is charged. The callback compares `binding_verifier` in constant time **after** burning `state` and **before** contacting the provider; a mismatch is the same `401 error.auth.oidc_state_invalid`. The SDK also refuses a foreign `state` locally (RFC 6749 §10.12).
- **ID-token checks:** alg ∈ {RS256, ES256, EdDSA}, never `none` or HMAC; signature against the provider JWKS; `iss` exactly `OIDC_ISSUER`; `aud` contains the client id; `azp` if present; `exp`/`nbf`/`iat` with 60 s skew; `nonce` matches; bounded `sub`. Every failure is one wire code, `401 error.auth.oidc_token_invalid`, with detail in the log. A provider refusing the exchange is `401 error.auth.oidc_exchange_failed`; an unreachable one is `500 error.auth.oidc_unavailable`, distinct from `error.auth.unavailable`.
- **Discovery is lazy**: first use, cached 1 day, so an IdP outage never blocks local auth. It is refused if its `issuer` differs from the configured one (mix-up defence). Endpoints must be `https` unless the issuer is a loopback IP literal, in which case plain-HTTP endpoints must also be loopback. JWKS is refetched on an unknown `kid` at most once a minute, and unconditionally after an hour to drop revoked keys. `OIDC_CA_BUNDLE` adds trust anchors.
- **Accounts are keyed on `(issuer, subject)`, never linked by address.** First sign-in creates a password-less account. A **verified** IdP email already held by any account is `409 error.auth.oidc_address_taken`; unverified emails reserve and collide with nothing. Explicit linking is a separate, out-of-scope ceremony.
- **A federated account is a row in the one account table with a null credential.** Registering over its verified address is `409 error.auth.user_already_exists`. A null credential never authenticates and is never charged to lockout. A profile with no verified address omits `email`.
- **OIDC ignores the password lockout**, or password guessing could lock a person out of SSO too.
- **A confirmed TOTP factor is honoured**: the callback answers the same `202` challenge.
- **Scopes are fixed at `openid email`.** `profile` is not requested.
- **`server-info` publishes `auth.oidc: { authorize, callback }` or `null`**, endpoints only, never issuer or client id. Unconfigured, the OIDC routes answer `404 error.auth.oidc_not_configured`.

**Configuration** (`capsule-server/src/config.rs`): `OIDC_ISSUER` (absent = off), `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET` (optional; absent = public PKCE-only client, RFC 8252 §8.5), `OIDC_REDIRECT_URL`, `OIDC_ALLOW_LOOPBACK_REDIRECT`, `OIDC_CA_BUNDLE`. An issuer without a client id, or the reverse, is a startup fault. Ceremonies live in PostgreSQL (in-memory in development), and `capsule-server/compose.yaml` ships a [dex](https://github.com/dexidp/dex) service (`--profile oidc`).

#### OIDC-Only Deployments

`PASSWORD_AUTH_DISABLED` (default off) narrows the audited surface to the identity provider.

- **`serve` refuses it without `OIDC_ISSUER`**, since a server with neither door admits nobody.
- **Register, login and reauthenticate answer `403 error.auth.password_disabled`** before anything else: no rate-limit charge, body validation, directory lookup or Argon2. A malformed body, an unknown address and a locked account get the same answer.
- **Every local account is refused**, including one created before the switch. There is no break-glass account: a named exception keeps a password door open on the account an attacker most wants, and the operator's recovery is turning the switch off.
- **Existing sessions are not revoked.** A password session lives out its sliding and hard expiry but cannot re-authenticate with a password, so freshness-gated operations are closed to it.
- **`server-info` publishes `auth.password_disabled: true`** beside `auth.oidc`, so a client hides its password form. Absent means `false`, as on a server predating the switch.

#### Signing In From the CLI

`capsule auth login --oidc` is the RFC 8252 native-app flow and needs no extra server surface.

- It listens on `127.0.0.1` at an ephemeral port with redirect `http://127.0.0.1:{port}/callback`. The server must enable loopback redirects, and the CLI prints that remedy on refusal.
- The URL is always printed. It is also opened unless `--no-browser`, only if it parses as absolute `http(s)`, and on Linux/BSD only under a graphical session.
- It waits ≤5 min, never past the ceremony deadline.
- Only a loopback callback carrying this ceremony's `state` ends the wait; anything else is ignored, so another local process cannot cut the sign-in short. A provider `error` for this `state` ends it, and `error_description` is printed with control characters stripped.
- The session is settled by the same function as a password sign-in, TOTP prompt included.

#### Signing In Without a Browser

Headless clients use the device authorization grant (RFC 8628): `capsule auth login --oidc --device`.

- **The server proxies the provider and the `device_code` never leaves it.** Begin takes a `binding_challenge` and answers a server-minted 128-bit `handle`, `user_code`, `verification_uri` (+ `_complete` if given), `interval` and `expires_by` (the minimum of the provider's `expires_in` and 15 min).
- **Polling** takes `handle`, `binding_verifier` and the advisory `cohort_hash`/`device_id`. Success answers as the callback does (tokens or `202`). Otherwise:

  | Provider answer | Capsule answer | Ceremony |
  | --- | --- | --- |
  | `authorization_pending` | `400 error.auth.oidc_device_pending` | kept |
  | `slow_down`, or poll sooner than one interval | `429 error.auth.oidc_device_slow_down`, interval += 5 s | kept |
  | `access_denied` | `401 error.auth.oidc_device_denied` | burned |
  | `expired_token`; unknown/spent handle; binding mismatch | `401 error.auth.oidc_device_expired` | burned |
  | other refusal | `401 error.auth.oidc_exchange_failed` | burned |
  | unreachable | `500 error.auth.oidc_unavailable` | kept |

- **The interval is server-enforced** at the provider's value, ≥5 s, and an early poll is refused without asking the provider. A client that waits one interval before every poll and adds 5 s per `slow_down` stays in step.
- **The ID token passes every callback check except the nonce, which MUST be absent**: a token carrying a nonce was minted for another authorization request. The back-channel request with a server-held device code replaces the nonce's binding.
- **`binding_verifier` is checked in constant time on every poll** before the provider is asked; a mismatch burns the ceremony.
- A session, TOTP challenge, refused ID token or taken address also burns the ceremony. It is burned before the session opens, so racing polls open one.
- **No grant support** (no `device_authorization_endpoint`, or `unauthorized_client`/`unsupported_grant_type`) is `404 error.auth.oidc_device_unsupported`; other device-endpoint refusals are `500 error.auth.oidc_unavailable`. `verification_uri` is held to the endpoint scheme rule, and `user_code` to ≤64 chars with no control characters.
- **Begin checks** binding shape first, then one deployment-wide 60/min budget, then store capacity. Polls are bounded by the begin budget × one provider call per interval.
- The CLI prints the address and code, never opens a browser, and gives up at `expires_by`.

## Identity and Discovery

Handles are `user@server.tld` (Matrix-style). **`.well-known/` never enumerates users.**

- `.well-known/capsule/server-info` is public and server-scoped only: API base, auth and federation endpoints, server signing key, `protocol_version` range and `min_protocol_version` cutoffs.
- **User lookup is authenticated**: local clients by session token, federated peers by a federation capability token ([](/design/federation/#federation-capabilities)) rate-limited per peer.
- **Anonymous WebFinger** returns only what the user opted into (default nothing), limited to handle and display name, never keys, devices or album hints, per the deny-by-default rule ([](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).

### The `.well-known/capsule/*` Registry

All are public and take no credential. A new path MUST add a row.

| Path | Contents | Owner |
| --- | --- | --- |
| `server-info` | Server-scoped facts above. Never a user list. | [](#identity-and-discovery) |
| `moved/{user}` | IK-signed moved certificate (post-v1). | [](#account-portability) |
| `revoked-jti` | Federation capability revocations (≤24 h). | [](/design/federation/#token-lifecycle-and-chain-of-trust) |
| `deprecation` | Min-supported-client announcements. | [](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy) |
| `attestation-keys` | Storage-attestation public keys + append-only history. | [](/design/transfer/durability/) |

## Account Portability

**Post-v1.** The User IK ([](/design/cryptography/keys/#user-identity-keys-user-iks)) is already a server-independent root; only the handle is host-bound. No DID system.

- The new server registers the account under the **same IK**, leaving the key hierarchy unchanged.
- The old server publishes `.well-known/capsule/moved/{user}`, `{ old_handle, new_handle, moved_at, ik_sig }`, cacheable 24 h. It is the only well-known record naming a user, admissible because the user signs and initiates it.
- Resolvers verify `ik_sig` against the IK from the signed device directory ([](/design/cryptography/keys/#device-directory)) and re-resolve. An unverifiable certificate is ignored (the old handle keeps resolving) and surfaced.

No server can forge a migration, because the IK signs the move.

## Session and Access Tokens

### Session ID

A server-generated UUIDv7 identifying the session record.

### Session Token

An opaque **128-bit** bearer secret (not a JWT) stored on the client, used only to obtain access tokens ([](#access-token)).

### Access Token

A short-lived **EdDSA JWT** signed with the server's Ed25519 key and issued against the session token. Classical only, per the operational-signature carve-out ([](/design/cryptography/primitives/#signature-scheme)).

## Session Expiry and Revocation

A session token stops being honoured on sliding expiry, hard expiry or explicit revocation, and nothing else. Expiries are enforced at access-token issuance.

### Sliding Inactivity Expiry

180 days (`SESSION_TTL_DAYS`) without an access-token issuance. Each issuance resets the clock; activity alone does not, since a request records last-used time and never extends the session.

### Hard Expiry

365 days (`SESSION_HARD_CAP_DAYS`) from sign-in, **never reset**. It caps the window for a silently exfiltrated token on an active device (the malicious keyholder of [](/design/threat-model/#client-class-taxonomy)).

Both are whole days, set per deployment and never per session. Zero, a negative count, or a cap shorter than the sliding window refuses the boot as a configuration fault. A stored session keeps the deadline it was opened under, so a changed value applies from its next refresh. A lowered value then refuses a session idle longer than the new window, or a lineage older than the new cap, as expired, and rotates one inside both with only what the new values leave it. A raised value lengthens nothing until that refresh.

Invariants:

- **The cap survives rotation.** The session record carries its lineage's sign-in instant; a refresh carries it forward unchanged and a re-authentication does not move it.
- **A session lives until the earlier of the two expiries**, and its refresh token is issued for exactly that remaining life, never a whole window past the cap.
- **A deploy never shortens a live session.** A record that predates the sign-in instant measures its cap from its own creation.

### Explicit Revocation

The session ledger supports:

1. **List** active sessions with last-used time.
2. **Revoke one** session, authenticated by any active session token.
3. **Revoke all**, authenticated by **proof of master-key possession**, not a session token, so a stolen token cannot lock the user out of every device.

Revoke-all is two requests. An authenticated `POST /v1/auth/logout/all/challenge` issues a single-use challenge; it is authenticated so it is not an account-existence oracle. An **unauthenticated** `POST /v1/auth/logout/all` redeems an IK signature over a domain-separated message containing it, verified against the account's identity anchor ([](/design/cryptography/keys/#device-directory)), never a request-supplied key. The challenge is burned on every attempt.

Invariants:

- **Revocation is immediate.** The bearer scheme reads the ledger on every authenticated request, so an in-flight access token is refused on next use. The access-token lifetime bounds a *lost* token, not a *revoked* one.
- **An unreadable ledger refuses** (fail closed). It currently renders `401`, and `POST /v1/auth/refresh` returns `error.auth.unavailable` so clients can tell outage from expiry. Open: the honest `503`.
- **Last-used is coarse**: at most one write per minute per session.
- **The server MUST NOT revoke sessions on its own initiative.** Doing so bypasses the user-initiated audit trail and is a bug.

## Device Cohorts

Reinstalling re-enrolls with a new `device_id`, because device keys are hardware-bound ([](/design/metadata/#add-id-binding)), so one phone accumulates ledger entries. The **cohort hash** groups sessions from one physical device and is a grouping aid only.

### Per-Platform Primary Identifier

| Platform | Primary identifier | App reinstall | OS reinstall | Factory reset |
| --- | --- | --- | --- | --- |
| iOS | Keychain random 128-bit seed, `ThisDeviceOnly`, non-synchronized | yes (not guaranteed) | no | no |
| Android | SSAID (signing-key-scoped `ANDROID_ID`) | yes | no | no |
| macOS | `IOPlatformUUID` | yes | yes | yes |
| Windows | `MachineGuid` | yes | no | no |
| Linux | `/etc/machine-id` (only ever hashed) | yes | no | no |

The promise is **reinstall-stable everywhere, reset-stable only on macOS**. A factory-reset phone is a new cohort.

### Encoding

```text
cohort_hash = SHA-256( canonical-CBOR([ "capsule-device-cohort/v1", user_id, platform_tag, primary_id ]) )
```

The input is canonical CBOR ([](/design/metadata/#canonical-cbor-encoding)) with a closed `platform_tag` enum. `user_id` is folded in so one device under two accounts yields unlinkable hashes. It is implemented once in `capsule-core` (`cohort`).

### Privacy and Scope

- Sent **only** in the session-opening request body, and registered in the header census ([](/design/threat-model/validation/#universal-headers)) as a body field. **Never** in signed artifacts, to federated peers or in `.well-known`.
- **Advisory only.** It is client-asserted and unverifiable, so **no authorization or capability decision may read it**, and it never substitutes for `device_id` or the DSK. Absent or garbage values behave identically to valid ones.

### Server Storage and Surfacing

- The session record carries `cohort_hash`, and a durable `device_cohorts(user_id, cohort_hash, first_seen, last_seen)` map outlives session expiry.
- Recording a cohort **never fails a sign-in**: it is written after the session, and a failure is logged and dropped.
- It appears only in the session listing. There is no filter by cohort and no "revoke cohort" verb; revocation names a `session_id`.
- The UX asserts ("a device you've used before, last seen *date*") with no "not my device" toggle. Disputes go through a support report bundling `{cohort_hash, [(device_id, session_id, first_seen, last_seen)]}`.

## Validation

- **Token round-trip (unit):** session token → access JWT → verify; after key rotation, old JWTs verify within their grace window.
- **Expiry (unit, mock clock):** sliding refreshes on each issuance, hard does not and survives rotation; rejection happens at access-token issuance.
- **Revoke-all (unit):** rejected without master-key proof; with it, every other session is invalidated.
- **Revocation immediacy (unit):** a revoked session's live access token is refused on next use.
- **TOTP:** `202` on a confirmed factor; code replay refused; per-challenge attempt cap; store outage fails closed.
- **OIDC (integration):** an in-process loopback mock provider (offline, container-free) covers key rotation, the refetch floor, a wrong PKCE verifier, every claim refusal, binding mismatch and each device-grant answer; gated round trips run against dex (`CAPSULE_TEST_DEX=1`).
- **Account portability (smoke, post-v1):** moved certificate from A; B registers the same IK; peers follow the move.
- **Cohort:** cross-language known-answer vectors; the same `primary_id` under two `user_id`s gives distinct hashes; absent, malformed and colliding values behave identically; a tripwire asserts no signed schema has a cohort field; the durable map outlives sessions.

The auth → library-query E2E case is in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).

## Related

- [](/design/authorization/): the closed lifecycle-action set every write proves against.
- [](/design/device-enrollment/): how a device joins the account and the device directory ([](/design/cryptography/keys/#device-directory)).
- [](/design/backup-recovery/): recovering the master key after device loss.
