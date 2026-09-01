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

## What is owed

There is **no binary, no configuration loading, and no Postgres or Valkey adapter.** Nothing reads
`JWT_ED25519_DER`, `SYNC_CURSOR_MAC_KEY` or `ATTESTATION_KEY_SEED` yet, so there is no way to run
this server outside its own tests. See `SLICES.md`, lane C.
