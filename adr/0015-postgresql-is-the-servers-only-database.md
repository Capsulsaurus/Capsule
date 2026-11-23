# ADR-0015 — PostgreSQL is the server's only database; Valkey is retired

- **Status:** accepted
- **Date:** 2026-11-10
- **Supersedes:** — (amends [ADR-0006](0006-the-server-is-a-binary-with-config-and-three-adapters-per-port.md): its Valkey adapters and the "never PostgreSQL" rule for state ports)
- **Superseded by:** —
- **Contract:** [Server Filesystem — Required Services](../capsule-docs/src/content/docs/design/filesystem/server.md#required-services)
- **Issues:** #1214, #403, #519

## Context

ADR-0006 gave every state port two adapters: Valkey (`redis-rs`) for "volatile" state and PostgreSQL for durable state, each with an in-memory double. It rejected a Postgres-only design because emulating TTLs in SQL "rebuilds the generic TTL store S-C29 deleted".

What Valkey came to hold was not volatile. It held sessions (read on every authenticated request; revoke-all depends on the per-user index), upload-session records (committed ranges, finalize claims), the challenge, enrollment, relay and OIDC ceremonies, every rate budget, the federation replay nonces (one-hit counter windows), and the federation pull leases. It had to run with `noeviction` and an append-only file, because an evicted key reopened a replay window or made "revoke all" revoke nothing (#519). Every caller fails closed, so an unreachable Valkey was a total outage with no degraded mode (#1214). Operators ran, sized, persisted and monitored a second database whose loss still signed everyone out. Its Lua scripts compute keys from values they read, so Redis Cluster was out of scope and Valkey was a single node at every size anyway.

Nothing has been released, so the choice can be revisited without a migration story for deployments.

Measured traffic on the hot path, before this decision (`routes/upload.rs`, `upload/chunk.rs`): one chunk `PATCH` made six Valkey round trips (session read, two counter hits, two upload-session reads, one committed-range write), one blob `fsync`, and no PostgreSQL access. A chunk is only ever written by the process that owns the blob root: in-flight range claims and the finalize claim are in-process (`upload/chunk.rs`, `upload/finalize.rs`).

### Engines considered for the whole server

Capsule's Postgres use is narrow but specific: transaction-scoped advisory locks (8 sites), `SELECT … FOR UPDATE`, one `LOCK TABLE`, `ON CONFLICT` upserts, `RETURNING (xmax = 0)`, `DISTINCT ON`, array binds, `COLLATE "C"` ordering, `REPEATABLE READ` read-only snapshots. It uses no JSONB, LISTEN/NOTIFY, `SKIP LOCKED`, full-text search or pgvector, and every instant is a `BIGINT` of microseconds.

- **SQLite embedded in the server.** It is the simplest option and runs in memory for tests. It allows one writer, cannot be shared by replicas, and cannot sit on network storage. It fits a single-node deployment and nothing larger.
- **Distributed SQLite (rqlite, dqlite, LiteFS, libSQL `sqld`).** Each puts one writer behind Raft or a single primary. rqlite documents unbatched write throughput of tens to hundreds of requests per second. All of them keep SQLite's single-writer ceiling and add a consensus round trip.
- **Turso's Rust rewrite of SQLite.** It adds MVCC concurrent writes. The engine is pre-1.0 (beta), and concurrent writes were an early preview in 2026.
- **MySQL or MariaDB.** It would need the same full rewrite: `GET_LOCK` is session-scoped rather than transaction-scoped, and there is no `DISTINCT ON`, no statement-level transition-table triggers, and no `RETURNING` in MySQL. It buys no scaling Postgres lacks.
- **A key-value store (redb, fjall, FoundationDB).** Every secondary index and transaction would be hand-written. That is more code to test and goes against the minimalism rule.

### What Synapse taught

Synapse (the Matrix reference server) started on SQLite and had to make PostgreSQL mandatory at scale. Its growing pains came from:

- tables that grew without bound (state groups, event edges);
- a per-request path that touched the database for data that never needed durability;
- worker replication routed through Redis, a side store that became a second database to operate.

## Decision

PostgreSQL is the server's only database. Every port Valkey backed gets a Postgres adapter that passes the same conformance suite as its in-memory double, and Valkey, `redis-rs` and `VALKEY_URL` are removed.

Each port keeps its own typed tables. A record carries its own `expires_at` (microseconds). Every read treats a record whose `expires_at` is at or before the injected clock's `now` as absent. A sweeper deletes expired rows in bounded batches. There is still no generic TTL or compare-and-set abstraction: ADR-0006's rejection of a grab-bag store stands, and only its "never PostgreSQL" rule is reversed.

Two write-path rules keep the database off the byte path:

1. **A committed range is recorded with asynchronous commit.** `record_range` runs in a transaction with `SET LOCAL synchronous_commit = off`, so a chunk costs one write-ahead-log append, not a second `fsync`.
   - A crash can lose the last few acknowledged ranges. The client's next `HEAD` then reports them uncommitted and it re-sends them; finalize re-hashes the whole blob before anything becomes durable.
   - The rule is never lose or corrupt data, and this loses neither.
   - Opening, claiming, failing and discarding a session stay synchronous.
2. **The two per-chunk throughput budgets are counted in-process.** `UPLOAD_CHUNKS` and `UPLOAD_CHUNKS_SOURCE` are routed to a process-local counter. Every other budget is counted in Postgres, exactly, across replicas. Those include the replay nonces, enrollment, TOTP, OIDC, registration, share, drop, federation, and the per-create `UPLOAD_SESSIONS*`.

### Risk analysis for the in-process budgets

- **What they protect.** Disk and CPU throughput per account and per source address. The budget's own documentation claims no exactness beyond the atomic hit, and no security argument rests on it, unlike `ENROLLMENT_REDEMPTION` or the nonce windows.
- **Why it is exact today.** A chunk succeeds only on the process that owns the session's blob root, so every successful chunk is counted by one counter.
- **Worst case with N replicas.** An attacker spreads requests across replicas and gets N times the budget, and the requests that land on a non-owning replica fail with `NotStaged` after their body is read. That amplification is still bounded by:
  - authentication on every request;
  - the exact, Postgres-counted `UPLOAD_SESSIONS*` budget at create;
  - 1000 live sessions per account;
  - 16 in-flight ranges per session;
  - the 32 MiB request body cap;
  - the quota reservation at create.
- **Resets.** A restart resets the counters. An attacker cannot cause one.
- **Alternatives rejected.**
  - memcached, or any shared cache, is another stateful service with Valkey's failure modes.
  - Postgres for these two budgets would add two writes to every chunk on the hottest path, for exactness the threat does not need.
- **When to revisit.** When uploads run on more than one process per blob root, these two budgets move to Postgres or the router pins an account to its owning process. The multi-replica upload issue tracks this.

### Scaling posture

- **Small.** One server process, one Postgres, one blob volume: two stateful parts instead of three. An idle Postgres container with small `shared_buffers` holds tens of megabytes.
- **Large** (100k+ assets per account, up to 100k+ accounts):
  - a larger instance first;
  - then read replicas for the read-heavy sync feed, which already reads under `REPEATABLE READ READ ONLY` snapshots;
  - then partitioning or sharding by `owner_id`, which every hot table already carries.
- **Rules carried from Synapse:**
  - the byte path stays off the database;
  - every table that grows with traffic has a stated bound or retention;
  - no second store relays state between replicas;
  - global serialization points are named. The custody-receipt `LOCK TABLE` is one, and a follow-up issue tracks it.

## Consequences

- `VALKEY_URL` is gone, and a durable boot needs only `DATABASE_URL`. A value left in the environment is ignored with a warning.
- Sessions, ceremonies, counters and pull leases are now in the Postgres backup. A restore brings sessions back rather than signing everyone out. The backup drill asserts this.
- Postgres takes:
  - one primary-key read per authenticated request;
  - one asynchronous-commit update per chunk;
  - one upsert per security-budget hit.

  It no longer gets a dedicated memory budget's worth of headroom from a side store, so self-hosting sizing moves Valkey's memory to Postgres.
- The container test tier needs one image, `postgres:18`, instead of two.
- `.claude/rules/capsule-server.md` and [Server Filesystem](../capsule-docs/src/content/docs/design/filesystem/server.md#required-services) drop the "never PostgreSQL" rule. ADR-0006's other decisions (one binary, one config loader, typed ports, in-memory doubles, `serve` never migrates) stand.

## Considered and rejected

- **Keep Valkey and add HA (Sentinel).** That is a third system to operate. Every invariant still fails closed on it.
- **Single-node SQLite server.** Rejected for scale-up; recorded above.
- **Make the in-memory adapters a production profile.** State would not survive a restart, and replicas could not share sessions.
