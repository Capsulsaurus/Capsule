# ADR-0006 — The server is one binary with one configuration loader and three adapters per port

- **Status:** proposed
- **Date:** 2026-09-01
- **Supersedes:** —
- **Superseded by:** —
- **Slices:** S-C29, S-C59, S-P7

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
ceremony stores (`ChallengeStore`, `EnrollmentStore`, `ChannelStore`) — each with **three
adapters**: PostgreSQL, Valkey through `redis-rs`, and the in-memory double.

Three adapters are not three deployment modes. Valkey is required and the binary refuses
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
