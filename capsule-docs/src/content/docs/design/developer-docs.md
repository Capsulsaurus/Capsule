---
title: Developer Documentation
description: How Capsule's developer surfaces become published reference pages
status: draft
---

Capsule publishes one docs site, built from `capsule-docs/` with [Starlight](https://github.com/withastro/starlight).
This doc owns how each developer surface becomes a page on it. Which surfaces exist is
[](/design/api-surfaces/#surfaces); the rules server code follows are [](/development/api-practices/).
A stale reference page is worse than a missing one, because it is confidently wrong, so none is
maintained by hand.

## Rules

1. **Artifacts cross the boundary, not toolchains.** The CI `docs` job installs bun alone. Each
   surface's own gate emits a small committed, machine-readable artifact, and the docs build reads
   only those files, never cargo, uniffi or wasm-bindgen. The `docs` path filter lists every
   artifact the build reads, not just `capsule-docs/**`.
2. **Reference pages are generated, never written.** Under `/reference/`, hand-written prose is one
   overview per section; `capsule-docs/scripts/gen-reference.mjs` emits the rest into gitignored
   output. A wrong page is fixed at its source (a doc comment, a `clap` `about`, a schema
   description) and regenerated.
3. **Freshness is a gate.** Every artifact has a `--check` mode that fails on drift, run in the
   owning toolchain's gate, never the docs gate.
4. **One chrome.** A generator that emits Starlight pages beats a renderer that mounts its own app:
   an embedded OpenAPI viewer escapes search, the link validator and the theme. The `Reference`
   sidebar is curated in `scripts/reference-groups.mjs`; generated pages never decide navigation
   order.

## Surfaces

| Surface | Artifact | Drift gate | Pages |
| --- | --- | --- | --- |
| REST | `capsule-server/openapi.json` (OpenAPI 3.2) | `openapi-check-kynos` | [](/reference/api/) |
| CLI | `capsule-cli/cli-surface.json` | `cli-surface-check` | [](/reference/cli/) |
| Swift and Kotlin | `capsule-sdk/bindings-surface.json` | `gen-bindings --check` (Rust gate) | [](/reference/sdk/swift/), [](/reference/sdk/kotlin/) |
| Browser | `capsule-wasm/wasm-surface.d.ts` | `wasm-surface-check` (web gate) | [](/reference/sdk/wasm/) |
| Rust crates | rustdoc, rendered from master, not committed | `doc-check-rust` | [GitHub Pages](https://capsulsaurus.github.io/Capsule/), mapped by [](/reference/sdk/rust/) |

- **Binding dump.** uniffi offers no stable machine-readable description of a namespace, so the
  dump is this repository's format: the declarations the native clients consume, as the generated
  sources spell them. It fails when one is missing.
- **Browser digest.** The caller-facing half of the declaration file wasm-bindgen emits, kept
  verbatim. Its check runs in the web gate, the only job that installs wasm-bindgen.
- **Rustdoc is the one exception to one chrome.** The workspace is `publish = false`, so docs.rs
  never builds it, and rustdoc emits a whole site of its own. The `rustdoc-pages` CI job publishes
  the public items of every crate except the generated migration and entity crates to GitHub Pages
  on each push to master (`mise run doc-publish-rust`). The Rust SDK page stays a Starlight page
  that links into it.
- **Not reference surfaces:** the retired `capsule.sync.v1` proto, `capsule_sdk::rest` (generated
  internals; REST is documented once, as its OpenAPI pages), and `legacy-review/`.

## Information Architecture

- `/reference/`: one row per surface, with its artifact and drift gate.
- `/reference/api/`: the REST overview, the generated conventions page (the document's own
  description), then one generated page per endpoint family.
- `/reference/cli/`: `capsule(1)`.
- `/reference/sdk/`: the Rust page (hand-written, linking into rustdoc), then the generated Swift,
  Kotlin and browser pages.

## No Playground

The server is key-free: writes are sealed on the client and the sync feed returns opaque
envelopes, so a try-it panel would teach that the API accepts plaintext. Executable examples live
where sealing happens: SDK doctests and CLI transcripts.

## Deferred

- Per-endpoint lifecycle metadata (stability, permission, changelog) as spec vendor extensions.
- Versioned docs (`starlight-versions` is installed but off) until the REST contract stabilizes.
- Reference internationalization ([](/design/i18n/)).
- Deploying the docs site automatically; it is published with `bun run deploy`.

## Validation

- `mise run check-docs` formats, lints, tests and builds the site; the link validator fails on a
  dead cross-reference.
- Each artifact's `--check` runs in its owning toolchain's gate.
- Adding a reference surface means reviewing the CI `docs` path filter.
