---
title: Authentication
description: Identity, account portability, session and access tokens
status: draft
---

Authentication binds a user identity to their master key, which is the root of every encryption and decryption operation in Capsule. The server can prove "this request is from a session it issued" but cannot prove "this user is who they say they are" — the master key, owned client-side, is the actual identity root. Everything below works to keep that binding intact through the lifetime of a session and across server moves.

Planned in `capsule-api::auth`: OIDC handling, the session ledger, claim validation, and per-device records. The retired Salvo implementation remains under `legacy-review/server-salvo/auth/` as review material, not an active server. The session token format and OIDC discovery surface below are the contracts other components — including federated peers — will depend on.

## Design Principles

- **Two first-class auth paths.** Local auth (password + TOTP, passkeys) and OpenID Connect are both first-class login methods — see [Choosing an Auth Path](#choosing-an-auth-path). The OIDC relying-party implementation is slice `S-N1`; local auth is the default path a deployment gets without configuring an IdP.
- **Cryptographic binding.** The user's identity is cryptographically bound to their master key. The server never sees the plaintext master key.

## Account Types

- **Registered accounts.** Associated with a unique identity and have their own master key. Authenticated using password+TOTP, passkeys, or OIDC ([Choosing an Auth Path](#choosing-an-auth-path)); the login credential authenticates the session while the master key stays cryptographically bound to the user.
- **Delegated/sponsored accounts.** Encrypted with keys derived from a registered account's master key. They do not have their own identity and rely on the registered account for authentication and key management. Owners of the sponsored account have full access. See [Cryptography — Keys: Delegated/Sponsored accounts](/design/cryptography/keys/#delegatedsponsored-accounts) for the key derivation.
- **Non-registered accounts.** No associated identity or master key — used for [share links](/design/share-links/), where the decryption keys are encapsulated around the secret stored in the link, and for [web-upload links](/design/web-upload/), where a guest seals contributions to a link-scoped key without read access.

## Choosing an Auth Path

Both paths mint the same Capsule [sessions](#session-and-access-tokens) and bind identity to the master key the same way; the difference is who verifies the login credential (decision 2026-07-12):

- **Local auth** (password + TOTP, or passkeys) — recommended for personal and self-hosted single-user or household servers: no external dependency, the server is self-contained.
- **OIDC** (external identity provider, authorization-code + PKCE) — recommended for enterprise and organizational deployments that already run an IdP, and for anyone wanting SSO. Capsule is a relying party only; account lifecycle policy lives at the IdP.

A deployment may enable either or both. Neither path weakens the cryptographic binding: the IdP (or password) authenticates the *session*; the master key never derives from, and is never visible to, the credential verifier.

## Identity and Discovery

Patterns borrowed from Matrix 2.0, with one critical departure: **`.well-known/` never enumerates the user list**. A federated setting where a peer can list every user on a server is unacceptable — both from an abuse-surface perspective (spam, harassment-target discovery, account-enumeration attacks) and a privacy perspective.

- All users have a handle like `user@yourserver.tld` (resembling Matrix's MXID pattern).
- `.well-known/capsule/server-info` is **public** and returns only server-scoped facts: the API base URL, auth endpoints, the federation endpoint, the server's signing key, supported `protocol_version` range, and `min_protocol_version` cutoffs for active deprecation windows. It **never** returns a user list.
- **User lookup is authenticated.** A client or peer server must present credentials to resolve `user@server.tld`:
  - **Local client lookup** (resolving another user on the same server, e.g. for sharing): authenticated by the looker's session token.
  - **Federated peer lookup** (resolving a user across servers): authenticated by a federation capability token (see [Federation — Federation Capabilities](/design/federation/#federation-capabilities)) and rate-limited per peer.
  - **Anonymous WebFinger**: returns only records the target user has explicitly opted into making public. The default is opt-out: no anonymous record. The opt-in-able record set is deliberately tiny — handle and display name only, never keys, device lists, or album hints; anything richer requires authenticated lookup. This is deliberately stricter than Matrix's default and follows the [deny-by-default rule](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar) from the threat model.

### The `.well-known/capsule/*` Registry

Every well-known path Capsule serves, in one census. Each path's record format is owned by the linked doc; a new path MUST add a row here when introduced.

| Path                                 | Contents                                                                                                                        | Owner                                                                                                   |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `.well-known/capsule/server-info`    | Public server-scoped facts: API base URL, auth + federation endpoints, server signing key, supported `protocol_version` range, deprecation cutoffs. Never a user list. | this doc ([Identity and Discovery](#identity-and-discovery))                                            |
| `.well-known/capsule/moved/{user}`   | The IK-signed moved certificate for a migrated account.                                                                          | this doc ([Account Portability](#account-portability))                                                  |
| `.well-known/capsule/revoked-jti`    | Federation capability revocation list (bounded to ≤ 24 h of revocations).                                                        | [Federation](/design/federation/#token-lifecycle-and-chain-of-trust)                                    |
| `.well-known/capsule/deprecation`    | Min-supported-client deprecation announcements.                                                                                  | [Threat Model — Schema Rules](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy) |
| `.well-known/capsule/attestation-keys` | The server's storage-attestation public keys + append-only key history.                                                        | [Storage Verification](/design/import/storage-verification/)                                              |

**Status note.** `attestation-keys` is part of the v1 server contract (specified, and exercised end-to-end against the pre-teardown server); `server-info`, `revoked-jti`, and `deprecation` land with slice `S-C18`; `moved/{user}` is post-v1 with [Account Portability](#account-portability). All of them are served by the planned `capsule-api` Kynos surface.

## Account Portability

**Status: post-v1** (decision 2026-07-12). No moved-certificate route or migration flow ships in v1; the contract below is normative for when it lands.

A user must be able to move servers without losing their identity. Capsule does **not** need a separate DID system: the user identity key (User IK — see [Cryptography — Keys](/design/cryptography/keys/#user-identity-keys-user-iks)) is *already* a server-independent root of trust. Only the `user@server.tld` handle is host-bound.

Migration re-homes the handle while keeping the same IK:

- The new server registers the account under the same IK; nothing in the [key hierarchy](/design/cryptography/keys/) changes.
- The old server publishes an IK-signed **moved certificate** at `.well-known/capsule/moved/{user}` — a small signed record `{ old_handle, new_handle, moved_at, ik_sig }`, cacheable for 24 hours. This is the one well-known record that names a specific user — opted-into (the user actively migrates) and carrying the user's own signature, so it does not constitute the kind of enumeration leak we forbid.
- Clients and [federated](/design/federation/) peers that resolve the old handle fetch this certificate, verify `ik_sig` against the IK they already trust for the user (from the signed [device directory](/design/cryptography/keys/#device-directory)), and re-resolve to the new handle it names. An unverifiable certificate is ignored — the old handle keeps resolving as before, and the failure is surfaced.

Because the IK signs the move and every device cross-signs to that IK, no server — old or new — can forge a migration or hijack the handle.

## Session and Access Tokens

These are the two token shapes consumers depend on. Both will be issued by `capsule-api::auth::session` after a successful authentication ceremony.

### Session ID

Sessions are identified by a UUIDv7 generated by the server upon successful authentication. It tracks session state and associated metadata.

### Session Token

A long-lived **128-bit secret** generated by the server upon successful authentication and stored securely on the client. It is **not a JWT** — it is an opaque bearer secret. The session token's only purpose is to obtain [access tokens](#access-token) for API requests.

### Access Token

Short-lived tokens issued against the session token (presented, not cryptographically derived), used to authenticate API requests. They have a limited lifespan and are refreshed using the session token without re-authenticating the user.

Capsule uses **EdDSA JWTs** as access tokens, signed under the server's Ed25519 signing key — classical only, per the [operational-signature carve-out](/design/cryptography/primitives/#signature-scheme) (access tokens are short-lived, so PQ hybridization buys no margin).

## Session Expiry and Revocation

Sessions expire in two ways: **sliding inactivity expiry** (automatic) and **explicit revocation** (user-initiated). They coexist; either causes the session token to stop being honored.

### Sliding Inactivity Expiry

A session that has not been used for **180 days** (default; deployment-configurable) expires automatically. "Used" means a successful [access-token](#access-token) issuance against the session token — each issuance refreshes the inactivity clock. This bounds the lifetime of a session on a device the user has forgotten about (a phone in a drawer, a laptop given to a relative) without forcing re-authentication on actively-used devices.

### Hard Expiry

Every session token has a **hard expiry of 365 days** from issuance (default; deployment-configurable). The hard expiry **does not reset** on use — it is the upper bound on the lifetime of a token regardless of activity.

The rationale is the malicious-keyholder class from [Threat Model — Client Class Taxonomy](/design/threat-model/#client-class-taxonomy): an attacker who silently exfiltrates a session token from a device the user actively uses would otherwise have an indefinite window of access. The hard expiry caps that window at one year; the user re-authenticates (passkey / password+TOTP) at most once a year per device — acceptable friction in exchange for a bounded leak-window.

Both expiries are enforced server-side at access-token issuance; the session token itself is not invalidated for any other reason than these expiries or an explicit revoke.

### Explicit Revocation

A common user session ledger supports:

1. **List all active sessions** (with last-used timestamp, so an expiring session is visible).
2. **Revoke any single session** by invalidating its session token — authenticated by any active session token.
3. **Revoke all sessions at once** ("log out of all devices") — authenticated by **proof of master-key possession** (a signature with the user's IK over a server-issued challenge), not by an active session token.

The asymmetric authentication on (3) addresses a damage scenario that pure session-token auth opens up: an attacker holding a stolen session token could otherwise invoke "log out of all devices" and lock the legitimate user out of every other device. Requiring master-key proof for the global revoke means an attacker with a session token can only revoke *that* session — they cannot escalate to denial-of-service. A user who has lost their master key is no worse off: they can still revoke individual sessions one at a time. The single-session revoke (2) is the everyday tool; the global revoke (3) is the nuclear option, gated accordingly.

Note: the server can theoretically just kick off sessions because session tokens are stored server-side and the server holds the encrypted data. But this should not ever be implemented and an attempt to do so would be a bug — it bypasses the audit trail of a user-initiated revoke.

## Device Cohorts

The session ledger has a legibility problem: reinstalling the app re-enrolls with a **new** `device_id` by design (device keys are hardware-bound and non-exportable — [Metadata, Add-id Binding](/design/metadata/#add-id-binding)), so one physical phone accumulates several ledger entries over its life and the user cannot tell them apart. The **device cohort hash** groups sessions from the same physical device. It is a grouping aid, nothing more.

### Per-Platform Primary Identifier

One primary identifier per platform — fewer, better-chosen inputs beat a concatenated fingerprint that splits whenever any component shifts:

| Platform | Primary identifier | Survives app reinstall | OS reinstall | Factory reset |
| --- | --- | --- | --- | --- |
| iOS | Keychain-persisted random 128-bit cohort seed (`ThisDeviceOnly`, **non-synchronized** — iCloud sync would merge distinct physical devices) | yes in practice (not Apple-guaranteed) | no | no |
| Android | SSAID (app-signing-key-scoped `ANDROID_ID`) | yes | no | no |
| macOS | `IOPlatformUUID` | yes | yes | **yes** |
| Windows | `MachineGuid` | yes | no | no |
| Linux | `/etc/machine-id` (never used raw — hashed below, per systemd guidance) | yes | no | no |

(IDFV was rejected for iOS: it resets on reinstall when no sibling app remains — precisely the case cohorts exist for.)

**Honest scope.** "The same device even after a factory reset" is technically impossible on iOS and Android: a reset destroys every app-accessible identifier by OS design, and post-reset attestation (DeviceCheck, Play Integrity) yields verdicts, not identifiers. The promise is therefore: **reinstall-stable everywhere; reset-stable only where the OS allows (macOS)**. A factory-reset phone starts a new cohort, and the doc says so rather than pretending otherwise — the same honesty rule as [platform limitations](/design/import/download-sync/#platform-limitations). (DeviceCheck's per-device bits may later corroborate a boolean "this hardware enrolled before"; deliberately out of scope for v1.)

### Encoding

```text
cohort_hash = SHA-256( canonical-CBOR([ "capsule-device-cohort/v1", user_id, platform_tag, primary_id ]) )
```

Domain-separated, [canonical CBOR](/design/metadata/#canonical-cbor-encoding) (never naive string concatenation), with a closed `platform_tag` enum. **`user_id` is folded in** so the same physical device under two accounts yields unlinkable hashes — the cross-account correlation surface is removed at the source. The pure function lives in `capsule-core` (`cohort` module) so every platform computes it identically.

### Privacy and Scope

- Sent **only** in the session-creation request body during the auth ceremony; **never** in signed artifacts (manifests, sidecars, the device directory), never to federated peers, never in `.well-known`. It is registered in the [`X-Capsule-*` header census](/design/threat-model/validation/#universal-headers) by pointer as a body field so no header variant drifts into existence.
- **Advisory-only, structurally:** the value is client-asserted and unverifiable, so **no authorization or capability decision may read it** — otherwise it becomes spoofable attack surface. It never substitutes for `device_id` (random UUIDv4, security-bearing) or the DSK. A server that receives an absent or garbage cohort value behaves identically to one that receives a valid one.

### Server Storage and Surfacing

The session record carries `cohort_hash`, and a small durable `device_cohorts(user_id, cohort_hash, first_seen, last_seen)` map persists it beyond session expiry — session-store-only would forget cohorts exactly when the "seen before" question matters. The session-listing surface returns the cohort per session plus the cohort map; clients group the ledger by cohort.

### UX and Support Contract

The client **asserts, it does not litigate**: "a device you've used before (last seen *date*)" — there is deliberately no "this isn't my device" toggle, because the user cannot adjudicate a hash and the value is advisory anyway. The dispute path is a **support report**: one tap bundles `{cohort_hash, [(device_id, session_id, first_seen, last_seen)]}` — the exact hash and device-id map — for a bug report.

## Validation

- **Token issuance round-trip (unit).** Generate a session token; issue an access JWT from it; verify the JWT under the server's Ed25519 key. Repeat with rotated keys; assert old JWTs verify under the old key for their grace window.
- **Expiry enforcement (unit).** Mock the clock; assert sliding expiry refreshes on use, hard expiry does not. Assert an expired token is rejected at access-token issuance, not earlier or later.
- **Revoke-all master-key proof (unit).** Issue a revoke-all without master-key proof; assert rejection. With proof; assert success and invalidation of every other session.
- **Login flow (smoke).** Full OIDC handshake against a testcontainer IdP; assert session token issued, persisted, and usable for an immediate access-token request. Re-run after a server restart; assert resilience.
- **Account portability (smoke).** Issue a moved certificate from server A; assert server B can register the same IK; assert federated peers honor the move after fetching A's well-known.
- **Cohort hash vectors (unit).** Known-answer vectors for `cohort_hash` through the cross-language canonical-CBOR conformance gate; same `primary_id` under two `user_id`s → distinct hashes.
- **Cohort is advisory (unit).** Session creation with an absent, malformed, or colliding cohort value behaves identically to a valid one; a tripwire test asserts no signed-structure schema contains a cohort field.
- **Cohort grouping (smoke).** Two sessions with one cohort group together in the session listing; a reinstall (new `device_id`, same cohort) groups with "previously used"; the durable map outlives session expiry.

The cross-module case — auth → query library schema — is one bounded E2E test listed in [Module Map](/design/module-map/#e2e-test-surface).

## Related

- [Authorization](/design/authorization/) — the closed lifecycle-action set every write proves against.
- [Device Enrollment](/design/device-enrollment/) — how a device joins the account and the [device directory](/design/cryptography/keys/#device-directory).
- [Backup & Recovery](/design/backup-recovery/) — recovering the master key and account after device loss.
