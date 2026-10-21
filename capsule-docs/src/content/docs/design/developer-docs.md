---
title: Developer Documentation
description: How Capsule's developer surfaces become published reference pages
status: draft
---

Capsule publishes **one docs site**. This doc owns how each developer surface becomes a page on it:
which artifact describes the surface, which gate keeps that artifact current, and where the page
lands. Which surfaces exist is [API Surfaces](/design/api-surfaces/#surface--transport-map); the
layering rules server code obeys are [API Practices](/development/api-practices/).

Implemented in `capsule-docs/` (Astro + Starlight); pages are generated from committed artifacts by
`capsule-docs/scripts/gen-reference.mjs`. A stale reference page is worse than a missing one — it is
confidently wrong — so no reference page is maintained by hand.

## Rules

The CI `docs` job installs **bun only** (no Rust, uniffi, or wasm-bindgen) and runs when its path
filter matches. That forces the boundary:

1. **Artifacts cross the boundary, not toolchains.** Each surface's owning gate emits a small,
   committed, machine-readable **description artifact**. The docs build reads committed artifacts
   and never invokes cargo, uniffi, or wasm-bindgen. The CI `docs` path filter MUST list every
   artifact the docs build reads, not just `capsule-docs/**`. Large output (rendered rustdoc) is
   built by the Rust gate and deployed beside the site, never committed.
2. **Reference pages are generated, never written.** Under `/reference/`, hand-written prose is one
   overview page per section. Everything else is emitted into gitignored build output; a wrong page
   is fixed in the source annotation (doc comment, clap `about`, schema description), then
   regenerated.
3. **Freshness is a gate.** Every artifact has a `--check` mode that fails on drift, run in the
   **owning toolchain's** gate, never the docs gate.
4. **One chrome.** Prefer a generator that emits Starlight pages over a renderer that mounts its own
   app (an embedded OpenAPI SPA escapes search, the link validator, and the theme). The `Reference`
   sidebar group is hand-curated in `astro.config.mjs`; generated pages never determine navigation
   order.

## Surfaces

| Surface | Artifact | Emitted by | Drift gate | Page | Status |
| --- | --- | --- | --- | --- | --- |
| REST | Kynos OpenAPI 3.2 document | `gen_openapi` | `openapi-check-kynos` | `/reference/api/` | Implemented |
| CLI | command-tree JSON | `gen_cli_surface` | `cli-surface-check` | `/reference/cli/` | Implemented |
| Rust SDK | rustdoc HTML (uncommitted) | `cargo doc -p capsule-sdk` | broken intra-doc links denied | `/reference/sdk/rust/` → `/reference/crates/` | Planned |
| Swift / Kotlin bindings | uniffi surface JSON dumped from the cdylib | a dump step on `mise-tasks/gen-bindings` | `--check` on the dump | `/reference/sdk/swift/`, `/reference/sdk/kotlin/` | Planned |
| Browser | surface digest of `capsule_wasm.d.ts` | a dump step on `build-wasm` | `--check` on the dump | `/reference/sdk/wasm/` | Planned |

The binding outputs are gitignored, so each needs a small committed dump beside its generation step.

**Rustdoc is the one exception to one chrome:** the workspace is `publish = false` (no docs.rs) and
rustdoc emits a whole site, so it is linked out from `/reference/crates/`. `/reference/sdk/rust/`
stays a Starlight page (narrative, auth model, worked example) linking into rustdoc for item detail.

**Not reference surfaces:** the retired `capsule.sync.v1` proto
([API Surfaces](/design/api-surfaces/#why-restopenapi-only)); `capsule_sdk::rest` (generated
internals — REST is documented once, as the OpenAPI page); `legacy-review/`.

## Information Architecture

```text
/reference/
  index          one card per surface: artifact, generator, drift gate
  api/           REST, from the Kynos OpenAPI document
  cli/           capsule(1)
  sdk/
    rust/        capsule-sdk narrative; links into crates/
    swift/       uniffi namespaces
    kotlin/      uniffi namespaces
    wasm/        capsule-wasm browser surface
  crates/        workspace rustdoc (linked out)
```

## No Playground

The server is key-free: writes are sealed client-side and the sync feed returns opaque envelopes, so
an HTTP try-it panel would teach that the API accepts plaintext. Executable examples live where
sealing happens: SDK doctests and CLI transcripts.

## Deferred

- Per-endpoint lifecycle metadata (stability, permission, changelog) as spec vendor extensions.
- Docs versioning (`starlight-versions` is installed, commented out) until the REST contract
  stabilizes.
- Reference internationalization ([Internationalization](/design/i18n/)).
- Deploy automation (publishing is a manual `bun run deploy`); required before hosting rustdoc.

## Validation

`mise run check-docs` formats, lints, and builds the site; the link validator fails on a dead
cross-reference. Each artifact's `--check` runs in its owning toolchain gate. Adding a reference
surface means reviewing the CI `docs` path filter.
