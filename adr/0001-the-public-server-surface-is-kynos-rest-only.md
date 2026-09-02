# ADR-0001 — The public server surface is Kynos REST/OpenAPI, and nothing else

- **Status:** accepted
- **Date:** 2026-07-12
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [API Surfaces](../capsule-docs/src/content/docs/design/api-surfaces.md)
- **Slices:** S-G1, S-C59, S-C60

## Context

The server presented three public surfaces at once. A Salvo application served REST; an
`async-graphql` schema served `/v1/library`; and a `capsule.sync.v1` gRPC feed served
change discovery, reaching the browser through gRPC-web framing that `capsule-web`
implemented itself — 410 lines of hand-rolled varint and length-delimited Protobuf, written
to avoid pulling in a `protobuf-es`/`connect-web` toolchain to read a handful of fields.

Three surfaces meant three schemas, three negotiation stories, three error mappings and
three test harnesses over one set of business rules. The GraphQL schema was worse than
redundant: its resolvers presumed a server that can read content — people, faces, smart
tags, memories evaluated server-side — which the end-to-end-encrypted model makes
structurally impossible. It predated that model and never adapted to it.

## Decision

Capsule exposes one public transport: Kynos REST with a checked-in OpenAPI **3.2**
document. GraphQL and gRPC are retired architecture rather than compatibility surfaces,
and neither is restored. Rich content queries have no server surface at all, because a
key-free server cannot evaluate them; they run client-side over `library.sqlite`, fed by
the sync feed.

The document's version is pinned with `openapi_as(SpecVersion::V3_2)` rather than left to
the default emitter, which returns the *lowest* version expressing the API without loss —
a sound default for a document produced on demand, and the wrong one for a contract that is
committed and generated from. Left to follow the API, the committed contract would flip
3.1 → 3.2 the day the first streamed response landed.

## Consequences

- The Salvo tree, the GraphQL crate and the gRPC feed moved to `legacy-review/`, which is
  reference material and not a Cargo workspace. They may inform contract tests; they cannot
  be mounted.
- `capsule-web` issues `GET /v1/sync` and parses JSON. Its client rules — forward-version
  rejection, per-album anti-rewind — were already behind a transport seam and did not move.
- Two things the browser can no longer see, both the REST feed being *stricter*: a blob's
  MIME type, which was plaintext metadata about an encrypted blob and is no longer a field,
  and an "unspecified" change kind, which existed only because a proto3 enum defaults to
  zero when a field is absent.
- The signed manifest still travels as opaque canonical CBOR. Re-modelling it as wire
  fields would detach it from its signatures.
- `AGENTS.md` forbids reintroducing Salvo, GraphQL or gRPC, and `xtask architecture-check`
  enforces that against both member manifests and `[workspace.dependencies]`.
