# capsule-server

The Capsule server: one [Kynos](https://github.com/getkono/kynos) REST/OpenAPI application.

It replaces the Salvo tree that now sits in [`legacy-review/server-salvo/`](../legacy-review/server-salvo/REVIEW.md).
That was never a transport swap — the old wire-contract types were themselves salvo-typed — which
is why this is a rebuild with its own contracts rather than a port.

## What the framework buys

Kynos derives the description from the types the server runs on. There is no second declaration of
a status, a path parameter or a body shape, so there is nothing for the description to drift from.

That matters here specifically. The Salvo surface had **thirteen response variants that rendered a
status the published schema never declared**: a `Writer` said `423`, its `EndpointOutRegister`
never registered one, and the generated client could not map it. That defect is not expressible in
this crate, because the status is part of the return type — and the inverse is a test:
`assert_declared_responses_covered` fails on any response the document promises and no test has
made the server send.

## Structure

One application composed from cohesive internal modules — not separate public transports or
microservices. `routes` is the only module that knows about HTTP; everything under it is
framework-free and testable without a router, which is why the operator workers (`gc`, `scrub`)
have no wire surface at all.

Authentication state and upload-session state stay behind separate Capsule-owned ports with
Postgres, Valkey and deterministic in-memory adapters. There is no generic CAS, transfer or TTL
abstraction, and none is planned.

## Every adapter is in-memory

Every port here has a deterministic in-memory adapter and a conformance suite, and **no Postgres,
Valkey or filesystem adapter is written** except the blob store's. That is an ordering rather than
an omission: the contract and its suite are what a real adapter is written *against*, and a port
with two implementations before it has one suite is a port whose implementations will disagree.

It is also why this crate's whole test suite runs without a container.

Two of those adapters live beside the ports rather than in `tests/support/`: `auth::accounts_memory`
and `auth::totp`'s `InMemoryTotp`. The account ports' docs say a double in `src/` would be "a fake
credential directory shipped inside the server binary", and that reasoning is about a **double** —
`tests/support/mod.rs`'s, which accepts whatever password it was told to accept. These verify with
the same Argon2id helper (`auth::credential`) a Postgres adapter will, store PHC strings and no
plaintext, take the timing-equalized miss, and lock an account out after enough failures — for a
window, because no route and no operator command can clear a lockout, so one that never expired
would be a permanently lost account. What they lack is durability, which is what makes them a
development profile rather than a deployment.

## Running the tests

```bash
cargo nextest run -p capsule-server
```

`kynos::test::TestClient` drives a built `Service` directly — no socket, no port, no runtime
flavour. One test (`tests/sdk_client.rs`) binds an ephemeral port, because the property it proves —
that the **generated** SDK client round-trips the real router over TCP — is the one an in-process
client cannot.

## The served contract

`openapi.json` is the committed OpenAPI **3.2** document, emitted from the router's own types and
gated against drift by `mise run openapi-check-kynos`. `capsule-sdk` generates its typed client
from exactly these bytes, so the client and the server cannot describe different APIs.

```bash
mise run openapi-kynos          # regenerate
mise run openapi-check-kynos    # verify no drift
```

## Running it

```bash
mise run serve-memory   # a server you can point a client at
```

One binary, several subcommands:

```text
capsule-server [--config PATH] <SUBCOMMAND>
  serve       [--listen HOST:PORT] [--memory] [--blob-root PATH]
  gc          [--apply] [--grace-window-hours N] --memory --blob-root PATH
  purge       [--apply] [--limit N]              --memory --blob-root PATH
  scrub       [--deep] [--budget BYTES]          --memory --blob-root PATH
  gen-openapi [FILE] [--check]

`--memory` is written as required on the three operator commands because today it is: they
compare the index against the blob store, and the only index adapter written is the in-memory
one. Without it they refuse and say so. It becomes optional when #402 lands.
```

`config` reads every setting an operator decides — command-line flag over environment over
default — and reports **every** fault in one message, because an operator otherwise restarts the
process once per variable. `capsule-server/.env.example` is the full list. There is no
configuration file; `--config PATH` is accepted and refused with a sentence saying why.

A real deployment supplies **two** independent secrets: `JWT_ED25519_DER` signs session tokens,
and `ATTESTATION_KEY_SEED` signs custody receipts. The second is deliberately not derived from
the first — a receipt that verified under the operational key would let anything holding that key
manufacture custody evidence, and a different HKDF label over the same input is not a separation.
`serve --memory` derives it, because a development server's whole state is discarded on exit.

`boot::assemble` is the one composition root. `--memory` takes every in-crate adapter over a real
filesystem blob store; anything else refuses, so a deployment that forgot `VALKEY_URL` fails
closed rather than coming up on state it loses at the next restart.

Logs go to stderr. stdout is a data channel: `serve` writes one `listening on <url>` line there,
which is how a `--listen 127.0.0.1:0` caller learns its port.

`gc`, `purge` and `scrub` need a blob root and no key material at all. Dry run is the default for
the two that write; `scrub` mutates nothing and exits non-zero on a non-empty report.

## What is owed

**No Postgres or Valkey adapter.** `DATABASE_URL` and `VALKEY_URL` are read into the
configuration and no adapter consumes either, so `serve` without `--memory` refuses and names
the issue that will honour it: the account, album and index adapters are one issue and the
session and upload-session adapters another.

That ordering is deliberate rather than unfinished, for the reason above: the contract and its
conformance suite are what a real adapter is written *against*. What `--memory` therefore buys is
not durability — the blob store is the only durable half — but a running surface to write those
adapters against and to point a client at.
