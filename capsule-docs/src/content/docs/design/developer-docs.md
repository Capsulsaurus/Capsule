---
title: Developer Documentation
description: How Capsule's developer surfaces become published reference pages
audit: none
---

Capsule publishes one docs site, built from `capsule-docs/` with Starlight.
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
   owning toolchain's required CI gate, never the docs gate. `cargo test` repeats it for the REST
   and CLI artifacts. A local docs run warns, and banners the affected pages, when a source
   changed on the branch after its artifact was last written (`scripts/lib/staleness.mjs`); it
   never fails, and is skipped in CI.
4. **One chrome.** A generator that emits Starlight pages beats a renderer that mounts its own app:
   an embedded OpenAPI viewer escapes search, the link validator and the theme. The `Reference`
   sidebar is curated in `scripts/reference-groups.mjs`; generated pages never decide navigation
   order.

## Surfaces

| Surface          | Artifact                                     | Drift gate                         | Pages                                                    |
| ---------------- | -------------------------------------------- | ---------------------------------- | -------------------------------------------------------- |
| REST             | `capsule-server/openapi.json` (OpenAPI 3.2)  | `openapi-check-kynos`              | [](/reference/api/)                                      |
| CLI              | `capsule-cli/cli-surface.json`               | `cli-surface-check`                | [](/reference/cli/)                                      |
| Swift and Kotlin | `capsule-sdk/bindings-surface.json`          | `gen-bindings --check` (Rust gate) | [](/reference/sdk/swift/), [](/reference/sdk/kotlin/)    |
| Browser          | `capsule-wasm/wasm-surface.d.ts`             | `wasm-surface-check` (web gate)    | [](/reference/sdk/wasm/)                                 |
| Rust crates      | rustdoc, rendered from master, not committed | `doc-check-rust`                   | [rustdoc](/rustdoc/), mapped by [](/reference/sdk/rust/) |

- **Binding dump.** uniffi offers no stable machine-readable description of a namespace, so the
  dump is this repository's format: the declarations the native clients consume, as the generated
  sources spell them. It fails when one is missing. It also carries each language's
  hand-written platform adapters (`capsule-core-kotlin`, `capsule-core-swift`: the StrongBox,
  Secure Enclave and software key stores an app constructs), which the pages render after the
  generated declarations. Those sources are committed, so the same check catches their drift,
  and the Rust CI filter lists them.
- **Browser digest.** The caller-facing half of the declaration file wasm-bindgen emits, kept
  verbatim. Its check runs in the web gate, the only job that installs wasm-bindgen.
- **Rustdoc is the one exception to one chrome.** The workspace is `publish = false`, so docs.rs
  never builds it, and rustdoc emits a whole site of its own. The `Rustdoc` workflow publishes
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
- `/llms.txt`: the [llms.txt](https://llmstxt.org) entry point for LLM agents, linking plain-text
  bundles of the same pages: `/llms-full.txt`, `/llms-small.txt`, and one `/_llms-txt/<set>.txt`
  per topic set curated in `capsule-docs/src/lib/llms-sets.mjs`. Generated from the pages at
  build time, latest only.

## Hosting and Versions

- **One domain.** Cloudflare Pages builds every push through its Git integration, so no deploy
  credential lives in the repository. `master` is production, served at `/` as "Latest".
- **Rustdoc** is too large for Pages (22k files, one over 25 MiB), so GitHub Pages hosts it,
  latest only. Production serves it under `/rustdoc/` through a Pages Function.
- **One deployment per release.** A release freezes its docs as the branch `docs/vX.Y`, built
  once with base `/v/X.Y/` and served by production under that path. A patch release moves the
  same branch forward; nothing older is rebuilt, so each build is one version's size.
- **The switcher** reads `/versions.json` from production at runtime, so an old version lists
  the releases after it. Switching keeps the page when it exists in the target version.
- **What is published is verified.** Production builds only `master`, which takes only commits
  whose `CI / required` passed, so every artifact a published page reads has passed its gate.

The URL layout and the proxy mapping are `capsule-docs/src/lib/docs-version.mjs`.

## No Playground

The server is key-free: writes are sealed on the client and the sync feed returns opaque
envelopes, so a try-it panel would teach that the API accepts plaintext. Executable examples live
where sealing happens: SDK doctests and CLI transcripts.

## Why Not Dokka

The Kotlin page is laid out after Dokka (summary tables, member rows that link to
themselves, the documented types a signature names) but rendered by the generator, for four
reasons:

- **Toolchain.** Dokka reads the sources against a resolved Gradle classpath. The bindings are
  gitignored uniffi output, so the docs build would need Gradle, a JDK, cargo and uniffi, which
  breaks the first rule.
- **Chrome.** Its HTML is a site of its own, outside search, the link validator, the theme and
  the version switcher. Its own templates can be overridden, but it stays a second site. Its
  Markdown and Javadoc outputs are still alpha.
- **Noise.** It documents every public declaration in the generated file, uniffi's converters
  and buffers included, and `@suppress` cannot be added to a file nobody writes.
- **No gain in prose.** uniffi copies Rust doc comments verbatim, so Dokka would publish the
  same rustdoc paths (`Self::open`, `crate::ffi::…`). The generator resolves them to entries
  on the page instead.

The toolchain and chrome reasons hold for DocC and the Swift page too.

## Deferred

- Per-endpoint lifecycle metadata (stability, permission, changelog) as spec vendor extensions.
- Reference internationalization ([](/design/i18n/)).

## Validation

- `mise run check-docs` formats, lints, tests and builds the site; the link validator fails on a
  dead cross-reference.
- Each artifact's `--check` runs in its owning toolchain's gate. The bindings and browser checks
  need a cdylib and a wasm-bindgen build, so they run only there.
- Adding a reference surface means reviewing the CI `docs` path filter.
