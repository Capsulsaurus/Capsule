---
title: Authentication
description: Identity, account portability, session and access tokens
---

Authentication binds a user identity to their master key, which is the root of every encryption and decryption operation in Capsule. The server can prove "this request is from a session it issued" but cannot prove "this user is who they say they are" — the master key, owned client-side, is the actual identity root. Everything below works to keep that binding intact through the lifetime of a session and across server moves.

Implemented in `capsule-api-auth`: OIDC handler (`oidc`), session ledger (`session`), claim validation (`claims`), per-device records (`devices`). The session token format and the OIDC discovery surface below are the contracts other components — including federated peers — depend on.

## Design Principles

- **Minimal surface.** The full OpenID Connect specification is implemented so identity is offloaded to an external provider where the user prefers it.
- **Cryptographic binding.** The user's identity is cryptographically bound to their master key. The server never sees the plaintext master key.

## Account Types

- **Registered accounts.** Associated with a unique identity and have their own master key. Authenticated using password+TOTP or passkeys, which cryptographically bind the user to their master key.
- **Delegated/sponsored accounts.** Encrypted with keys derived from a registered account's master key. They do not have their own identity and rely on the registered account for authentication and key management. Owners of the sponsored account have full access. See [Cryptography — Keys: Delegated/Sponsored accounts](/design/cryptography/keys/#delegatedsponsored-accounts) for the key derivation.
- **Non-registered accounts.** No associated identity or master key — used for [share links](/design/share-links/), where the decryption keys are encapsulated around the secret stored in the link, and for [web-upload links](/design/web-upload/), where a guest seals contributions to a link-scoped key without read access.

## Identity and Discovery

Patterns borrowed from Matrix 2.0, with one critical departure: **`.well-known/` never enumerates the user list**. A federated setting where a peer can list every user on a server is unacceptable — both from an abuse-surface perspective (spam, harassment-target discovery, account-enumeration attacks) and a privacy perspective.

- All users have a handle like `user@yourserver.tld` (resembling Matrix's MXID pattern).
- `.well-known/capsule/server-info` is **public** and returns only server-scoped facts: the API base URL, auth endpoints, the federation endpoint, the server's signing key, supported `protocol_version` range, and `min_protocol_version` cutoffs for active deprecation windows. It **never** returns a user list.
- **User lookup is authenticated.** A client or peer server must present credentials to resolve `user@server.tld`:
  - **Local client lookup** (resolving another user on the same server, e.g. for sharing): authenticated by the looker's session token.
  - **Federated peer lookup** (resolving a user across servers): authenticated by a federation capability token (see [Federation — Federation Capabilities](/design/federation/#federation-capabilities)) and rate-limited per peer.
  - **Anonymous WebFinger**: returns only records the target user has explicitly opted into making public. The default is opt-out: no anonymous record. This is deliberately stricter than Matrix's default and follows the [deny-by-default rule](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar) from the threat model.

## Account Portability

A user must be able to move servers without losing their identity. Capsule does **not** need a separate DID system: the user identity key (User IK — see [Cryptography — Keys](/design/cryptography/keys/#user-identity-keys-user-iks)) is *already* a server-independent root of trust. Only the `user@server.tld` handle is host-bound.

Migration re-homes the handle while keeping the same IK:

- The new server registers the account under the same IK; nothing in the [key hierarchy](/design/cryptography/keys/) changes.
- The old server publishes an IK-signed **"moved" certificate** at its `.well-known/` path, naming the new handle. This is the one well-known record that names a specific user — opted-into (the user actively migrates) and carrying the user's own signature, so it does not constitute the kind of enumeration leak we forbid.
- Clients and [federated](/design/federation/) peers that resolve the old handle fetch this certificate, verify its IK signature, and re-resolve to the new handle it names.

Because the IK signs the move and every device cross-signs to that IK, no server — old or new — can forge a migration or hijack the handle.

## Session and Access Tokens

These are the two token shapes consumers depend on. Both are issued by `capsule-api-auth::session` after a successful authentication ceremony.

### Session ID

Sessions are identified by a UUIDv7 generated by the server upon successful authentication. It tracks session state and associated metadata.

### Session Token

A long-lived **128-bit secret** generated by the server upon successful authentication and stored securely on the client. It is **not a JWT** — it is an opaque bearer secret. The session token's only purpose is to obtain [access tokens](#access-token) for API requests.

### Access Token

Short-lived tokens derived from the session token, used to authenticate API requests. They have a limited lifespan and are refreshed using the session token without re-authenticating the user.

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

## Validation

- **Token issuance round-trip (unit).** Generate a session token; issue an access JWT from it; verify the JWT under the server's Ed25519 key. Repeat with rotated keys; assert old JWTs verify under the old key for their grace window.
- **Expiry enforcement (unit).** Mock the clock; assert sliding expiry refreshes on use, hard expiry does not. Assert an expired token is rejected at access-token issuance, not earlier or later.
- **Revoke-all master-key proof (unit).** Issue a revoke-all without master-key proof; assert rejection. With proof; assert success and invalidation of every other session.
- **Login flow (smoke).** Full OIDC handshake against a testcontainer IdP; assert session token issued, persisted, and usable for an immediate access-token request. Re-run after a server restart; assert resilience.
- **Account portability (smoke).** Issue a moved certificate from server A; assert server B can register the same IK; assert federated peers honor the move after fetching A's well-known.

The cross-module case — auth → query library schema — is one bounded E2E test listed in [Module Map](/design/module-map/#e2e-test-surface).

## Related

- [Authorization](/design/authorization/) — the closed lifecycle-action set every write proves against.
- [Device Enrollment](/design/device-enrollment/) — how a device joins the account and the [device directory](/design/cryptography/keys/#device-directory).
- [Backup & Recovery](/design/backup-recovery/) — recovering the master key and account after device loss.
