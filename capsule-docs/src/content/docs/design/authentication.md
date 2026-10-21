---
title: Authentication
description: Identity, account portability, session and access tokens
status: draft
---

The server can prove "this request is from a session it issued"; it cannot prove who the user is. The **master key**, held client-side, is the identity root: every login credential authenticates a *session* only, and the master key never derives from, and is never visible to, the credential verifier.

## Account Types

- **Registered accounts.** A unique identity and their own master key. Sign in with password (+ optional TOTP) or OIDC ([Choosing an Auth Path](#choosing-an-auth-path)).
- **Delegated/sponsored accounts.** Keys derived from a registered account's master key; no identity of their own, authenticated and managed by the sponsor, who has full access. See [Keys — Delegated/Sponsored accounts](/design/cryptography/keys/#delegatedsponsored-accounts).
- **Non-registered accounts.** No identity or master key: [share links](/design/share-links/) (keys encapsulated to the link secret) and [web-upload links](/design/web-upload/) (guest seals to a link-scoped key, no read access).

## Creating an Account

`POST /v1/auth/register` takes **an address and a password, nothing else**, and answers `200` with a token pair (registering signs you in).

- No display name, username or invitation code — the server stores as little about a person as it can.
- Password: a 12-character floor, no composition rule (it guards a session, not the master key).
- A taken address is `409 error.auth.user_already_exists`. This is an account oracle by decision; it is **not** rate-limited yet, because limiting an unauthenticated write needs a trusted client address the server lacks behind an unconfigured proxy chain (same gap as the share and drop source limiters).
- A new account has **no device directory and therefore cannot write**: invariant 7's floor is the writing device's `added_at` in the published directory, with no account-creation fallback. The client's first act is publishing a directory.
- Not provided: token introspection (the session ledger is read on every request, so validation *is* the request) and password reset (the server cannot re-wrap a master key it never saw; recovery is the [escrow blob](/design/backup-recovery/)).

### Passkeys Are Not in v1

No passkey surface ships. Building one is new work, and it reopens the [Dependencies](/design/dependencies/) `rustls`-only rule: `webauthn-rs` links `openssl` for attestation parsing.

### The Second Factor

TOTP is the local path's second factor.

| Endpoint | Contract |
| --- | --- |
| `POST /v1/auth/login` | `202 Accepted` with a challenge when the account has a **confirmed** factor. No session, cohort or refresh token exists until the challenge completes. |
| `POST /v1/auth/login/verify-totp` | Challenge + code; **opens the session** (so advisory `cohort_hash`/`device_id` ride this request). 5 attempts per **challenge**, not per account — a per-account budget lets anyone lock an owner out. |
| `POST /v1/auth/totp/enroll` | Issues secret + `otpauth://` URI. Gates nothing until confirmed. Refused over a confirmed factor; replaces a pending one. |
| `POST /v1/auth/totp/verify-enrollment` | Confirms; the confirming code is **spent** (enters the replay ledger). |
| `POST /v1/auth/totp/disable` | Requires a live code, not just a session — a stolen access token must not switch off the control. |

- **A code is accepted at most once** (RFC 6238 §5.2): compare-and-set on the highest step used. Parameters are fixed: SHA-1, 6 digits, 30 s step, ±1 step drift.
- **A store outage fails sign-in closed.**
- Clients read the `202` from the status; the SDK returns a `LoginOutcome`, never a challenge shaped like a failure.
- `POST /v1/auth/reauthenticate` takes a password alone: it is performed by an existing session, which has already passed the factor.

### The Profile Surface

- **`GET /v1/auth/profile`** — the caller's own id, address, optional display name, creation time: the whole of what the server stores about a person. No `{user_id}` segment, so reading another's profile is unrepresentable; other accounts' public facts are the [device directory](/design/cryptography/keys/#device-directory).
- **`PATCH /v1/auth/profile`** — display name only. Absent key = unchanged, explicit `null` = clear. Trimmed, ≤128 chars, control characters **refused**, not rewritten.
- **`POST /v1/auth/password`** — change, authenticated by the current password through the same path as sign-in (lockout applies). Closes **every** session of the account, then re-opens the caller's. Wrong current password is `403`, never `401`.
- **The login address is immutable.** Changing it without proof of control of the new address is an account takeover; the port has no method for it until such proof exists.

## Choosing an Auth Path

Both paths mint the same [sessions](#session-and-access-tokens); they differ only in who verifies the credential. A deployment may enable either or both.

- **Local auth** (password + optional TOTP) — default; no external dependency. For personal and household servers.
- **OIDC** (authorization code + PKCE; Capsule is relying party only) — for deployments with an IdP or wanting SSO. Account lifecycle policy lives at the IdP.

### Signing In Through an Identity Provider

Authorization code + PKCE, plus the device grant ([below](#signing-in-without-a-browser)). No implicit or hybrid flow.

**Ceremony.** `POST /v1/auth/oidc/authorize` takes the client's `redirect_uri` and `binding_challenge`; answers the provider URL, `state`, and a 10-minute deadline. The client receives the redirect itself. `POST /v1/auth/oidc/callback` takes `state`, `code`, `binding_verifier` and answers exactly as `POST /v1/auth/login` does (token pair or `202` TOTP challenge), via the same session code.

Rules:

- **Redirect allow-list.** Exactly `OIDC_REDIRECT_URL`, or — only when `OIDC_ALLOW_LOOPBACK_REDIRECT` is on (default off) — `http` on `127.0.0.1`/`[::1]`, any port (RFC 8252 §7.3; `localhost` refused per §8.3). Stored with the ceremony and replayed byte-for-byte to the token endpoint. Refusal: `400 error.auth.oidc_redirect_invalid`.
- **Begin is rate-bounded** (unauthenticated write): 60/min per admitted redirect host (`429 error.auth.rate_limited`), and the ceremony store's capacity (`503 error.auth.oidc_at_capacity`). The limiter key is chosen **after** redirect validation; all refusals share one deployment-wide bucket, so a caller cannot mint counter keys. Limiter ceilings are partitioned per key kind so a flood on one surface cannot starve another.
- **`state` is single-use**, burned on the first callback whatever the outcome. Unknown, spent and expired are one answer: `401 error.auth.oidc_state_invalid`.
- **Client binding (login-CSRF defence).** `binding_challenge` is the S256 hash (43-char unpadded base64url) of a ≥256-bit client secret; malformed → `400 error.request.malformed` before any budget is charged. The callback's `binding_verifier` is compared in constant time **after** burning `state` and **before** contacting the provider; mismatch → the same `401 error.auth.oidc_state_invalid`. The SDK also refuses a foreign `state` locally (RFC 6749 §10.12).
- **ID-token checks:** alg ∈ {RS256, ES256, EdDSA} (never `none`/HMAC); signature against provider JWKS; `iss` exact-equal to `OIDC_ISSUER`; `aud` contains client id; `azp` if present; `exp`/`nbf`/`iat` with 60 s skew; `nonce` matches; bounded `sub`. Every failure is one wire code, `401 error.auth.oidc_token_invalid` (detail goes to the log). Provider refuses exchange → `401 error.auth.oidc_exchange_failed`; unreachable → `500 error.auth.oidc_unavailable` (distinct from `error.auth.unavailable`).
- **Discovery** is lazy (first use, cached 1 day; an IdP outage never blocks local auth). Refused if its `issuer` differs from the configured one (mix-up defence). Endpoints must be `https` unless the issuer is a loopback IP literal, in which case plain-HTTP endpoints must also be loopback. JWKS refetched on unknown `kid` at most once a minute, and unconditionally after an hour (to drop revoked keys). `OIDC_CA_BUNDLE` adds trust anchors.
- **Accounts are keyed on `(issuer, subject)`, never linked by address.** First sign-in creates a password-less account. A **verified** IdP email already held by any account → `409 error.auth.oidc_address_taken`; unverified emails reserve and collide with nothing. Explicit linking is a separate, out-of-scope ceremony.
- **A federated account is a row in the one account table with a null credential.** Registering over its verified address is `409 error.auth.user_already_exists`. A null credential never authenticates and is never charged to lockout. A profile with no verified address omits `email`.
- **OIDC ignores the password lockout** — otherwise password guessing could lock a person out of SSO too.
- **A confirmed TOTP factor is honoured**: the callback answers the same `202` challenge.
- **Scopes are `openid email`**, fixed. `profile` is not requested.
- **`server-info` publishes `auth.oidc: { authorize, callback }` or `null`** — endpoints only, never issuer or client id. Unconfigured → `404 error.auth.oidc_not_configured`.

**Configuration** (`capsule-server/src/config.rs`): `OIDC_ISSUER` (absent = off), `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET` (optional; absent = public PKCE-only client, RFC 8252 §8.5), `OIDC_REDIRECT_URL`, `OIDC_ALLOW_LOOPBACK_REDIRECT`, `OIDC_CA_BUNDLE`. Issuer without client id, or vice versa, is a startup fault. Ceremonies live on Valkey (in-memory in development); `capsule-server/compose.yaml` ships a dex service (`--profile oidc`).

#### Signing In From the CLI

`capsule auth login --oidc` is the RFC 8252 native-app flow; no extra server surface.

- Listener on `127.0.0.1`, ephemeral port, redirect `http://127.0.0.1:{port}/callback` (server must enable loopback redirects; the CLI prints that remedy on refusal).
- The URL is always printed; opened too unless `--no-browser`, only if it parses as absolute `http(s)`, and on Linux/BSD only under a graphical session.
- Wait ≤5 min, never past the ceremony deadline.
- Only a loopback callback request carrying this ceremony's `state` ends the wait; anything else is ignored, so another local process cannot cut the sign-in short. A provider `error` for this `state` ends it; `error_description` is printed with control characters stripped.
- The session is settled by the same function as a password sign-in, TOTP prompt included.

#### Signing In Without a Browser

Device authorization grant (RFC 8628) for headless clients: `capsule auth login --oidc --device`.

- **The server proxies the provider; the `device_code` never leaves the server.** `POST /v1/auth/oidc/device` takes a `binding_challenge` and answers a server-minted 128-bit `handle`, `user_code`, `verification_uri` (+ `_complete` if given), `interval`, and `expires_by` (min of provider `expires_in` and 15 min).
- **`POST /v1/auth/oidc/device/poll`** takes `handle`, `binding_verifier`, advisory `cohort_hash`/`device_id`. Success answers as the callback does (tokens or `202`). Otherwise:

  | Provider answer | Capsule answer | Ceremony |
  | --- | --- | --- |
  | `authorization_pending` | `400 error.auth.oidc_device_pending` | kept |
  | `slow_down`, or poll sooner than one interval | `429 error.auth.oidc_device_slow_down`, interval += 5 s | kept |
  | `access_denied` | `401 error.auth.oidc_device_denied` | burned |
  | `expired_token`; unknown/spent handle; binding mismatch | `401 error.auth.oidc_device_expired` | burned |
  | other refusal | `401 error.auth.oidc_exchange_failed` | burned |
  | unreachable | `500 error.auth.oidc_unavailable` | kept |

- **Interval is server-enforced**: provider's value, ≥5 s; an early poll is refused without asking the provider. A client waiting one interval before every poll and adding 5 s per `slow_down` stays in step.
- **ID token: every callback check except the nonce**, which MUST be absent (a token carrying a nonce was minted for some other authorization request). The back-channel request with a server-held device code replaces the nonce's binding.
- **Binding**: `binding_verifier` checked in constant time on every poll before the provider is asked; mismatch burns the ceremony.
- A session, TOTP challenge, refused ID token or taken address also burns the ceremony (burned before opening the session, so racing polls open one).
- **No grant support** (no `device_authorization_endpoint`, or `unauthorized_client`/`unsupported_grant_type`) → `404 error.auth.oidc_device_unsupported`; other device-endpoint refusals → `500 error.auth.oidc_unavailable`. `verification_uri` held to the endpoint scheme rule; `user_code` ≤64 chars, no control characters.
- **Begin bound**: binding shape first, then one deployment-wide 60/min budget, then store capacity. Polls are bounded by the begin budget × one provider call per interval.
- The CLI prints the address and code, never opens a browser, and gives up at `expires_by`.

## Identity and Discovery

Handles are `user@server.tld` (Matrix-style). **`.well-known/` never enumerates users.**

- `.well-known/capsule/server-info` is public and server-scoped only (API base, auth and federation endpoints, server signing key, `protocol_version` range, `min_protocol_version` cutoffs).
- **User lookup is authenticated**: local clients by session token; federated peers by a [federation capability token](/design/federation/#federation-capabilities), rate-limited per peer.
- **Anonymous WebFinger** returns only what the user opted into (default: nothing), limited to handle and display name — never keys, devices or album hints. This follows the [deny-by-default rule](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar).

### The `.well-known/capsule/*` Registry

All are public and take no credential. A new path MUST add a row.

| Path | Contents | Owner |
| --- | --- | --- |
| `server-info` | Server-scoped facts above. Never a user list. | [Identity and Discovery](#identity-and-discovery) |
| `moved/{user}` | IK-signed moved certificate (post-v1). | [Account Portability](#account-portability) |
| `revoked-jti` | Federation capability revocations (≤24 h). | [Federation](/design/federation/#token-lifecycle-and-chain-of-trust) |
| `deprecation` | Min-supported-client announcements. | [Schema Rules](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy) |
| `attestation-keys` | Storage-attestation public keys + append-only history. | [Transfer — Durability](/design/transfer/durability/) |

## Account Portability

**Post-v1.** The [User IK](/design/cryptography/keys/#user-identity-keys-user-iks) is already a server-independent root; only the handle is host-bound. No DID system.

- The new server registers the account under the **same IK**; the key hierarchy is unchanged.
- The old server publishes `.well-known/capsule/moved/{user}`: `{ old_handle, new_handle, moved_at, ik_sig }`, cacheable 24 h. It is the only user-naming well-known record, admissible because the user signs and initiates it.
- Resolvers verify `ik_sig` against the IK from the signed [device directory](/design/cryptography/keys/#device-directory) and re-resolve. An unverifiable certificate is ignored (old handle keeps resolving) and surfaced.

No server can forge a migration: the IK signs the move.

## Session and Access Tokens

### Session ID

A server-generated UUIDv7 identifying the session record.

### Session Token

An opaque **128-bit** bearer secret (not a JWT), stored on the client. Its only use is obtaining [access tokens](#access-token).

### Access Token

A short-lived **EdDSA JWT** signed with the server's Ed25519 key, issued against the session token. Classical only, per the [operational-signature carve-out](/design/cryptography/primitives/#signature-scheme).

## Session Expiry and Revocation

A session token stops being honoured on sliding expiry, hard expiry, or explicit revocation — nothing else. Expiries are enforced at access-token issuance.

### Sliding Inactivity Expiry

180 days (configurable) without an access-token issuance. Each issuance resets the clock.

### Hard Expiry

365 days (configurable) from issuance; **never reset**. Caps the window for a silently exfiltrated token on an active device ([malicious keyholder](/design/threat-model/#client-class-taxonomy)).

### Explicit Revocation

The session ledger supports:

1. **List** active sessions with last-used time.
2. **Revoke one** session — authenticated by any active session token.
3. **Revoke all** — authenticated by **proof of master-key possession**, not a session token, so a stolen token cannot lock the user out of every device.

Revoke-all is two requests: authenticated `POST /v1/auth/logout/all/challenge` issues a single-use challenge (authenticated so it is not an account-existence oracle); **unauthenticated** `POST /v1/auth/logout/all` redeems an IK signature over a domain-separated message containing it, verified against the account's [identity anchor](/design/cryptography/keys/#device-directory), never a request-supplied key. The challenge is burned on every attempt.

Invariants:

- **Revocation is immediate.** The bearer scheme reads the ledger on every authenticated request, so an in-flight access token is refused on next use. The access-token lifetime bounds a *lost* token, not a *revoked* one.
- **An unreadable ledger refuses** (fail closed). It currently renders `401`; `POST /v1/auth/refresh` returns `error.auth.unavailable` so clients can tell outage from expiry. Open: the honest `503`.
- **Last-used is coarse**: at most one write per minute per session.
- **The server MUST NOT revoke sessions on its own initiative**; doing so bypasses the user-initiated audit trail and is a bug.

## Device Cohorts

Reinstalling re-enrolls with a new `device_id` (device keys are hardware-bound, [Add-id Binding](/design/metadata/#add-id-binding)), so one phone accumulates ledger entries. The **cohort hash** groups sessions from one physical device. It is a grouping aid only.

### Per-Platform Primary Identifier

| Platform | Primary identifier | App reinstall | OS reinstall | Factory reset |
| --- | --- | --- | --- | --- |
| iOS | Keychain random 128-bit seed, `ThisDeviceOnly`, non-synchronized | yes (not guaranteed) | no | no |
| Android | SSAID (signing-key-scoped `ANDROID_ID`) | yes | no | no |
| macOS | `IOPlatformUUID` | yes | yes | yes |
| Windows | `MachineGuid` | yes | no | no |
| Linux | `/etc/machine-id` (only ever hashed) | yes | no | no |

Promise: **reinstall-stable everywhere; reset-stable only on macOS.** A factory-reset phone is a new cohort.

### Encoding

```text
cohort_hash = SHA-256( canonical-CBOR([ "capsule-device-cohort/v1", user_id, platform_tag, primary_id ]) )
```

[Canonical CBOR](/design/metadata/#canonical-cbor-encoding), closed `platform_tag` enum. `user_id` is folded in so one device under two accounts yields unlinkable hashes. Implemented once in `capsule-core` (`cohort`).

### Privacy and Scope

- Sent **only** in the session-opening request body. **Never** in signed artifacts, to federated peers, or in `.well-known`; registered in the [header census](/design/threat-model/validation/#universal-headers) as a body field.
- **Advisory only:** client-asserted and unverifiable, so **no authorization or capability decision may read it**. Never substitutes for `device_id` or the DSK. Absent or garbage values behave identically to valid ones.

### Server Storage and Surfacing

- The session record carries `cohort_hash`; a durable `device_cohorts(user_id, cohort_hash, first_seen, last_seen)` map outlives session expiry.
- Recording a cohort **never fails a sign-in** (written after the session; failure logged and dropped).
- It appears only in the session listing: no filter by cohort, no "revoke cohort" verb — revocation names a `session_id`.
- UX asserts ("a device you've used before, last seen *date*") with no "not my device" toggle; disputes go via a support report bundling `{cohort_hash, [(device_id, session_id, first_seen, last_seen)]}`.

## Validation

- **Token round-trip (unit):** session token → access JWT → verify; after key rotation, old JWTs verify within their grace window.
- **Expiry (unit, mock clock):** sliding refreshes on use, hard does not; rejection happens at access-token issuance.
- **Revoke-all (unit):** without master-key proof rejected; with it, every other session invalidated.
- **Revocation immediacy (unit):** a revoked session's live access token is refused on next use.
- **TOTP:** `202` on confirmed factor; code replay refused; per-challenge attempt cap; store outage fails closed.
- **OIDC (integration):** in-process loopback mock provider (offline, container-free) covering key rotation, refetch floor, wrong PKCE verifier, every claim refusal, binding mismatch and each device-grant answer; gated round trips against dex (`CAPSULE_TEST_DEX=1`).
- **Account portability (smoke, post-v1):** moved certificate from A; B registers the same IK; peers follow the move.
- **Cohort:** cross-language known-answer vectors; same `primary_id` under two `user_id`s → distinct hashes; absent/malformed/colliding values behave identically; a tripwire asserts no signed schema has a cohort field; durable map outlives sessions.

The auth → library-query E2E case is in [Module Map](/design/module-map/#e2e-test-surface).

## Related

- [Authorization](/design/authorization/) — the closed lifecycle-action set every write proves against.
- [Device Enrollment](/design/device-enrollment/) — how a device joins the account and the [device directory](/design/cryptography/keys/#device-directory).
- [Backup & Recovery](/design/backup-recovery/) — recovering the master key after device loss.
