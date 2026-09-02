# ADR-0004 — `capsule-wire` is retired once no member depends on it

- **Status:** proposed
- **Date:** 2026-09-01
- **Supersedes:** —
- **Superseded by:** —
- **Slices:** S-C27, S-C59

## Context

`capsule-wire` exists because the Salvo server's response taxonomy — which outcome carries
which status, which body, which published sentence — lived only as framework trait impls,
written twice per response enum (once to render it, once to document it) with nothing
keeping the two halves in agreement. That made the transport load-bearing: the contract
could not outlive the framework. `S-C27` extracted the taxonomy as plain data
(`ResponseSpec`, `BodyShape`, `WireResponses`) plus the protocol headers, depending on
nothing but `serde`, and generated the framework impls from it. The crate's own module
comment names the goal: "the piece of the server that survives the transport swap
unchanged".

The transport swapped, and the crate did not survive it in the way the extraction
anticipated. Three facts in the tree say so:

- **Kynos removes the defect the crate was extracted to fix.** In Kynos the status *is*
  part of the return type and there is one declaration, so the two halves cannot disagree.
  `capsule-server/tests/conformance.rs` asserts both directions of that agreement against
  the emitted document rather than against a hand-kept table.
- **The server owns the taxonomy now.** `capsule-server::problem`, `::limits` and `::body`
  carry coded-problem bodies, body-size limits and the header census on every route — the
  Server Modules table in `design/module-map.md` lists them there.
- **Nothing links it.** `capsule-server/Cargo.toml` declares `capsule-wire` as a path
  dependency, and the only occurrence of `capsule_wire` anywhere in the workspace outside
  the crate itself is one prose reference in `capsule-server/src/lib.rs`'s module comment.
  Meanwhile `capsule-wire/src/salvo_adapter.rs` — a third of the crate — generates impls
  for a framework `S-C59` removed from the workspace.

## Decision

`capsule-wire` is retired. The header constants and any part of the taxonomy the server
still needs move into `capsule-server`, which is where the rest of it already lives; the
Salvo adapter goes with the framework it adapts; the crate leaves `[workspace] members`
and `xtask`'s architecture check gains it as a retired dependency so it cannot return.

The retirement lands when no workspace member depends on it, not before — a crate that is
still in a manifest is still in the build graph, whatever its call sites say.

## Consequences

- The protocol-header contract has one home, `capsule-server::problem` and its siblings,
  rather than two with an unenforced agreement between them.
- `S-C27` closes as a design that did its job and is no longer needed, rather than as a
  design that failed. Extracting the taxonomy is what made the Kynos port a port rather
  than a rewrite.
- One fewer workspace member, and one fewer manifest edge that `architecture-check` has to
  reason about.
- A future transport swap loses the framework-free layer this crate provided. That is
  accepted: Kynos's own contract — one declaration, checked by `conformance.rs` — is a
  stronger guarantee than a second crate holding a copy of the table.

## Considered and rejected

- **Keeping the crate and deleting only `salvo_adapter.rs`.** That leaves a crate whose
  only consumer is a doc comment. A dependency nothing calls is a maintenance cost with no
  reader.
- **Moving the taxonomy into `capsule-sdk` so the client owns it.** The taxonomy is a
  statement about what the *server* sends. Putting it on the client side would let the two
  drift in the direction that matters least.
