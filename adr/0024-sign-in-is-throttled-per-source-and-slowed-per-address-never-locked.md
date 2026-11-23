# ADR-0024 — Sign-in is throttled per source and slowed per address; no account is ever locked

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Authentication — Sign-In Limits](../capsule-docs/src/content/docs/design/authentication.md#sign-in-limits), with the numbers in [Authorization — Rate Limits](../capsule-docs/src/content/docs/design/authorization.md#rate-limits)
- **Issues:** #1210, #1001, #510, #528

## Context

Before this decision, the password doors were bounded by a **hard per-account lockout** and nothing else.

- `accounts.failures` and `accounts.last_failure_at` hold the run. One conditional `UPDATE` reserves an attempt before Argon2 runs, so a parallel burst cannot pass the ceiling (#510; `capsule-server/src/auth/accounts_postgres.rs`). The defaults are 10 failures and a 15-minute lock (`LOCKOUT_MAX_ATTEMPTS`, `LOCKOUT_WINDOW_SECONDS`; `capsule-server/src/config.rs`). Login, re-authentication and password change all refuse on `Locked` with `423 error.auth.account_locked`.
- Login had **no per-source limit**. The route's module docs still report that "the counter port that would back it does not exist here" (`capsule-server/src/routes/auth.rs`). That was true when written and stopped being true once the counter port shipped. Registration already charges a per-source budget (#1001).
- `budgets::LOGIN_ATTEMPTS` (5 per 15 min) and `CounterKey::LoginAttempts` exist and are charged nowhere. Their only reference outside the counter module is a comment explaining why login does not use them (`capsule-server/src/auth/directory.rs`).
- The second factor was budgeted per challenge only (`SECOND_FACTOR`, 5 per 5 min). Challenge issuance was unbounded, so a password holder minted fresh challenges and got five guesses each, without end (#1210 item 2). The design doc relied on "lockout" in three places and defined it nowhere (#1210 item 3).

A per-account lockout is a remote denial of service: anyone who knows an address locks its owner out for as long as they keep guessing. The TOTP section already rejected a per-account budget for exactly that reason. The lockout also gave a sign-in oracle. Only an existing account can reach `Locked`, so its `423` distinguishes a known address from an unknown one, and the activity log's coalescing of `sign_in.locked` events exists because strangers could cause them.

## Decision

No account is ever locked. Password sign-in is bounded by four counters, all in PostgreSQL `rate_counters` and all failing closed:

1. **Per source** (IPv4 address or IPv6 `/64`), charged on every parsed attempt before Argon2, on login, re-authentication, password change and registration.
2. **Per presented address, a soft exponential delay.** It is keyed on a keyed hash of the address string rather than on the account, so an unknown address is slowed exactly as a known one. An attempt inside the delay is refused unverified and uncharged.
3. **Per sign-in device token.** A device that has signed in to an address before is exempt from that address's delay and bounded by its own token's budget.
4. **Per session** for re-authentication and password change, which a session performs. A stranger's failures cannot block them, and a stolen session's guesses are bounded.

Every route that checks a TOTP code is charged per challenge **and** per account. The per-account budget cannot be spent by a stranger, because each such route requires the password or a session first. A completed challenge is burned.

## Consequences

- The `423` status, `error.auth.account_locked`, the `accounts.failures`/`last_failure_at` columns, `LOCKOUT_MAX_ATTEMPTS`, `LOCKOUT_WINDOW_SECONDS` and `MAX_FAILED_ATTEMPTS` are removed. `LOGIN_ATTEMPTS` and `CounterKey::LoginAttempts` are deleted or replaced by the new keys, never left dead. The stale "rate limiting has no port" comments in `routes/auth.rs`, `auth/directory.rs` and `auth/accounts_memory.rs` go with them.
- Login and verify-enrollment gain a `429`. Login and the sign-in response gain the sign-in device token.
- The fixed-window counter gains one new shape: a not-before delay that doubles per failure and decays. It is still one atomic charge-and-decide per hit.
- The activity log's "refused sign-ins are coalesced" rule keeps its bound. Its justification changes from "no budget is charged" to "the address delay is keyed off the account".
- **Residual risk.** An attacker who holds the delay at its cap from many sources slows a brand-new device's first sign-in to one attempt per 15 minutes. Devices that have signed in before are unaffected.
- Sign-in now depends on PostgreSQL for counting as well as for credentials and sessions. That adds no new failure mode, because the same database already holds the password hash.

## Considered and rejected

- **Keep the hard lockout and add a per-source limit.** The owner-denial remains, and per-source limits do nothing against a distributed lock-out.
- **A soft delay keyed on the account.** It slows only known addresses, so the delay itself answers "does this address exist", which is the oracle the single `401` exists to deny.
- **A delay without the device-token exemption.** It costs an attacker four requests an hour per address to hold an owner at one attempt per 15 minutes on every device.
- **Slow failures (sleep before answering wrong passwords).** A caller who drops the connection after the fast path's latency learns the answer without waiting, and a held connection is server resources spent on the attacker.
- **CAPTCHA or proof of work.** A CAPTCHA is a third-party dependency on every sign-in of a self-hosted server. Proof of work costs an attacker's botnet less than a phone's battery.
- **A per-account budget for TOTP, as originally rejected.** The rejection assumed a stranger could spend it, but every route that charges it sits behind the password or a session.
