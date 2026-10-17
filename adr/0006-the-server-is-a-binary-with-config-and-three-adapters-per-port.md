# ADR-0006 — The server is one binary with one configuration loader and two adapters per port

- **Status:** accepted (amended 2026-10-17)
- **Date:** 2026-09-01
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Filesystem — Server](../capsule-docs/src/content/docs/design/filesystem/server.md)
- **Slices:** S-C29, S-C59, S-P7

The file name keeps the "three adapters" it was recorded under, because numbers and names
are never reused; the [amendment](#amendment-2026-10-17) below is what the record now says.

## Context

`capsule-server` is a complete *surface* and not yet a program. `src/bin/` holds one
binary, `gen_openapi.rs`, which builds the router purely to describe it and needs no
database, no Valkey, no key material, no disk and no network. `src/store/` holds the typed
ports and one adapter — `memory.rs`, the deterministic test double — beside the
`conformance.rs` suite every adapter must pass. Nothing anywhere reads `JWT_ED25519_DER`,
`SYNC_CURSOR_MAC_KEY`, `VALKEY_URL` or `DATABASE_URL`: those names appear only in doc
comments describing what a loader will do. `design/development/local-development.md` states
the same thing plainly, and `mise run serve-api` — the one command that used to bring the
stack up — retired with the Salvo binary it launched in `S-C59`.

So nothing that needs a live server can run: not the CLI's networked commands, not the web
client beyond its empty states, not the iOS lane's end-to-end path, not the bounded E2E
cases. That single absence is the rebuild's critical path, and it is currently described
across a slice note, a design doc and two module comments rather than decided once.

`S-C29` already settled the *shape* of the state layer, and settled it against the
alternative worth naming. The Salvo server kept session records, the per-user session
index, MFA counters, rate-limit counters and a generic `save_temp_data<T>` /
`get_temp_data<T>` behind one `SessionStorage` trait with a caller-supplied TTL — a
serialize-anything key-value store that four unrelated ceremonies rode, namespaced by
hand-formatted string keys. `AGENTS.md`'s Rust Architecture Decisions refuse exactly that
abstraction, and `S-C29` deleted it rather than porting it: separate typed ports, no
`T: Serialize` anywhere, TTL a property of the store rather than an argument, and boxed
futures so every port stays dyn-compatible and an adapter can be swapped behind
`Arc<dyn …>` without making the server generic over its storage.

## Decision

The server is **one binary**, with **one configuration loader**, over the typed ports
`S-C29` defined — `AuthStateStore`, `UploadSessionStore`, `CohortStore`, and the three
ceremony stores (`ChallengeStore`, `EnrollmentStore`, `ChannelStore`) — each with **two
adapters**, and which two is a property of the port: the volatile ports get Valkey through
`redis-rs` and the in-memory double; `CohortStore`, the one record that outlives the
sessions that carried it, gets PostgreSQL and the in-memory double.

Two adapters are not two deployment modes. Valkey is required and the binary refuses
to boot without it; the in-memory adapter is a test double and never a deployment profile.
Whichever adapter is in play passes the one shared suite in `capsule-server::store::conformance`,
which is what makes "the in-memory double behaves like Valkey" an assertion rather than an
assumption — and what lets the rest of the rebuild be tested without a container.

No generic TTL or CAS abstraction is introduced to unify them. Blob storage and the
resumable encrypted upload protocol stay Capsule-owned behind narrow ports, and no
`object_store` or generic transfer crate is adopted to hold them.

## Consequences

- The configuration loader is the one place secrets enter the process. `JWT_ED25519_DER`
  is the root of that set: `sync/cursor.rs` HKDF-derives the cursor MAC key from it when
  `SYNC_CURSOR_MAC_KEY` is absent, and `discovery/` derives the rest, so a self-hoster
  supplies one value rather than a list.
- A Postgres adapter must satisfy the transactions finalization needs, row locking,
  migrations, cancellation, typed error mapping and tracing. A Valkey adapter must satisfy
  the atomic compare-and-update and expiry primitives each port requires. Neither may
  widen a port to make itself easier to write.
- Refusing to boot without Valkey is a deliberate operational cost. The rejected
  alternative — a Postgres fallback that removes Valkey — means emulating TTL and expiry
  in SQL, which rebuilds the generic TTL store `S-C29` deleted.
- A serve task returns to `mise`, replacing what `serve-api` did for the Salvo tree.
  Everything gated on a live server unblocks with it: `S-P7`'s successor, the E2E cases,
  the web client's non-empty states.

## Considered and rejected

- **Keeping the server library-only and driving it from tests.** It is what the tree does
  today, and it is why nothing outside the crate can exercise the rebuild. A description
  and a test harness are not a deployment.
- **One `SessionStorage`-style trait again.** The grab-bag whose deletion `S-C29` is; its
  caller-supplied TTL and `T: Serialize` payloads are the two properties the port shape
  makes inexpressible.
- **Postgres-only, with TTLs emulated in SQL.** Cheaper to operate and it reintroduces the
  generic TTL abstraction as an implementation detail nobody can see.

## Amendment (2026-10-17)

The binary landed (#479), and it took decisions this record did not state. Its review
found them unsanctioned; the maintainer sanctioned them as built (#522). The two that
concern this record's own subject — which adapters the binary binds, and how it boots
against the durable one — are recorded here as amendments to it. The other two are
decisions in their own right and have their own records, per the README's one ADR per
decision: [ADR-0007](0007-revoking-a-capability-revokes-every-successor-refreshed-from-it.md)
(revoking a capability reaches its refreshed successors) and
[ADR-0008](0008-the-sync-manifest-payload-is-an-unversioned-provenance-record.md) (the sync
payload is an unversioned `ProvenanceRecord`).

1. **Two adapters per port, not three, and Postgres rather than Valkey for cohorts.** No
   port binds all three in a deployment: the volatile stores bind Valkey, with the double
   in tests, and a Postgres session table is what [Filesystem — Server](../capsule-docs/src/content/docs/design/filesystem/server.md)
   refuses outright. `CohortStore` is the exception in the other direction — it has no
   expiry, so its durable adapter is Postgres (`PostgresCohorts`), bound by `durable_ports`
   in [`capsule-server/src/boot.rs`](../capsule-server/src/boot.rs). It is the one port
   with all three adapters written: `ValkeyCohorts` exists too, but no deployment binds it;
   it survives only so `capsule-server/tests/valkey.rs` can drive the whole port set on one
   connection. The adapter map is stated in the module docs of
   [`capsule-server/src/store/mod.rs`](../capsule-server/src/store/mod.rs), section
   "Adapters, and what they are for".
2. **`serve` never migrates.** It reads `seaql_migrations` and refuses a schema behind the
   one it was built for, naming `capsule-server-migration up`; an operator runs migrations
   explicitly, before the rollout. A server that migrated on start would race once per
   replica during a rolling deploy, and the server cannot link the migrator at all (its
   `chrono` gate). The check is `assert_schema_current` in
   [`capsule-server/src/postgres/mod.rs`](../capsule-server/src/postgres/mod.rs), called from
   `open_durable_database` in [`capsule-server/src/boot.rs`](../capsule-server/src/boot.rs).
   Contract: [Filesystem — Server](../capsule-docs/src/content/docs/design/filesystem/server.md),
   "Schema migrations".
