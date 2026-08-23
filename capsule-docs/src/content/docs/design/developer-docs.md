---
title: Developer Documentation
description: How Capsule's developer surfaces become published reference pages
status: draft
---

Capsule publishes one docs site. This document owns **how each developer surface becomes a page on
it** — which artifact describes the surface, which gate proves that artifact current, and where the
resulting page lands. It does not decide which surfaces exist: that is the
[API Surfaces](/design/api-surfaces/#surface--transport-map) map, and the layering rules server code
obeys are [API Practices](/development/api-practices/).

Implemented in `capsule-docs/` (Astro + Starlight) plus one description emitter per surface, living
in the crate that owns the surface and driven by its `mise` task. Every surface named below is
**Planned** or **Blocked** — `capsule-docs/src/content/docs/reference/` holds no generated content
today.

## The Problem

Capsule's developer surface is plural out of proportion to its size: a REST contract, a Rust SDK, two
sets of uniffi bindings, a wasm-bindgen browser surface, a command-line tool, and workspace rustdoc.
Six generators, four languages, three build systems. None of them is published.

The failure mode when that accumulates is a **second docs site**. Whatever renders OpenAPI does not
render rustdoc, and neither renders a clap command tree, so the reference acquires its own build, its
own subdomain, and its own component library. That trades drift-by-hand for drift-by-chrome: two
themes, two search indexes, and a navigation split the reader has to cross manually.

The single-site answer only works if the reference cannot rot, because a stale reference page is
worse than a missing one — it is confidently wrong. So the contract below is two commitments taken
together: **one site, and a generation discipline strict enough that no reference page is ever
maintained by hand.**

## The Constraint

The CI `docs` job installs **bun and nothing else**, and runs only when the `capsule-docs/**` path
filter matches. It has no Rust toolchain, no `uniffi-bindgen`, no `wasm-bindgen`.

Any design in which `astro build` shells out to `cargo doc` or a bindings generator breaks that job,
and — more quietly — makes the path filter a lie: a change to `capsule-cli` would leave the CLI
reference stale without failing anything. The constraint is not an inconvenience to route around; it
is what forces the boundary to sit in the right place.

## Three Rules

### 1. Artifacts cross the boundary, not toolchains

Each surface's owning gate emits a **description artifact**: a small, committed, machine-readable
file describing the surface. The docs build reads committed artifacts and nothing else. It never
invokes cargo, uniffi, or wasm-bindgen.

`capsule-sdk/openapi.json` already is such an artifact — emitted by a state-free binary, refreshed by
`mise run openapi`, and drift-gated by `mise run openapi-check`. This rule generalizes that shape to
every other surface rather than inventing a second mechanism for each.

Two consequences follow, and both are load-bearing:

- The CI `docs` path filter must list **every artifact the docs build reads**, not just
  `capsule-docs/**`. A filter that omits one lets a stale reference page publish.
- Size decides what gets committed. A description artifact is committed because it is small,
  diffable, and reviewable — a schema change should be visible in a pull request. Rendered rustdoc
  HTML is none of those things; it is built by the Rust gate and deployed as a sibling, never
  committed.

### 2. Reference pages are generated, never written

Under `/reference/`, hand-written prose is confined to **one overview page per section**: what the
surface is for, how to authenticate against or install it, and where its contract lives. Everything
below that overview is emitted from the artifact into gitignored build output.

A generated page is never edited. If a generated page is wrong, the annotation in the source is
wrong — fix the doc comment, the clap `about`, or the schema description, and regenerate. This is the
rule that lets reference documentation survive contributor turnover: correctness is a property of the
code, not of anyone's memory.

### 3. Freshness is a gate, not a habit

Every description artifact has a `--check` mode that regenerates it and fails on any difference, and
that check runs in the **owning toolchain's** gate — the Rust gate for Rust-derived artifacts — never
in the docs gate, which cannot run it.

## The Surfaces

| Surface | Description artifact | Emitted by | Drift gate | Page | Status |
| --- | --- | --- | --- | --- | --- |
| REST | Kynos OpenAPI 3.1 document | `capsule-server::openapi()` via an emitter binary | `openapi-check` | `/reference/api/` | **Blocked** |
| CLI | command-tree JSON, man pages, shell completions | `capsule-cli` (clap) | new `--check` on the dump | `/reference/cli/` | Planned |
| Rust SDK | rustdoc HTML (uncommitted) | `cargo doc -p capsule-sdk` | broken intra-doc links denied | `/reference/sdk/rust/` → `/reference/crates/` | Planned |
| Swift bindings | uniffi surface JSON, dumped from the compiled cdylib | a dump step on `mise-tasks/gen-bindings` | new `--check` on the dump | `/reference/sdk/swift/` | Planned |
| Kotlin bindings | as above | as above | as above | `/reference/sdk/kotlin/` | Planned |
| Browser | TypeScript surface digest of `capsule_wasm.d.ts` | a dump step on `mise run build-wasm` | new `--check` on the dump | `/reference/sdk/wasm/` | Planned |

**Why the binding surfaces need a new artifact.** The generated Swift and Kotlin bindings and the
wasm-bindgen `.d.ts` are all gitignored build output, so the bun-only docs build cannot read them.
Each therefore needs a small committed dump alongside its existing generation step. The
symbol-presence assertions already in `mise-tasks/gen-bindings` are the seed of that dump — they
already enumerate the verbs each binding must export — but they assert, they do not yet emit.

**Why REST is blocked.** The committed `capsule-sdk/openapi.json` is emitted from the retired Salvo
server. Its Kynos replacement exposes `openapi() -> Document` but has a single route ported and no
emitter binary, so no Kynos document exists yet. The REST reference is generated from the Kynos
document when there is one; publishing the Salvo-derived file would document a server nothing runs.

**What is deliberately not a reference surface:**

- The `capsule.sync.v1` proto and its gRPC-web framing. Both are
  [retired architecture](/design/api-surfaces/#why-restopenapi-only), and documenting them would
  advertise a compatibility surface that does not exist.
- `capsule_sdk::rest`. Spargen output, `include!`d from `OUT_DIR` under `allow(missing_docs)`, and an
  implementation detail behind `AuthenticatedClient`. The REST contract is documented once — as the
  OpenAPI page.
- `legacy-review/`, which is non-buildable reference material by construction.

## One Site, One Chrome

Reference pages are Starlight pages. The operative rule is: **prefer a generator that emits pages
over a renderer that mounts an application.**

An embedded OpenAPI single-page app ships its own stylesheet and its own router. Its endpoint text
never enters the site's search index, its links are invisible to the link validator, and its palette
tracks its vendor's brand rather than Capsule's. The reader gets a visibly foreign page and a search
box that cannot find a single endpoint. A Starlight-native generator costs some renderer polish and
buys all of that back — a page authored that way inherits, with no extra work:

- accent colours and typography from `src/styles/global.css`
- the review-status badge from the `PageTitle` override
- `translate="no"` on technical terms from the notranslate rehype pass
- dead-link detection from `starlight-links-validator`
- full-text search alongside the guides and design docs

**The rustdoc seam.** Rustdoc is the exception, and this doc states it rather than hiding it. The
workspace is `publish = false`, so docs.rs will never build it; rustdoc emits a complete themed site
rather than a page; and its output is far too large to commit. So it is built by the Rust gate,
deployed beside the site, and linked from `/reference/crates/` as an explicit departure. An
accent-matched `--extend-css` narrows the visual gap without closing it.

The mitigation is that `/reference/sdk/rust/` stays a real Starlight page — the narrative, the
authentication model, a worked example, and links into rustdoc only for item-level detail. A reader
who needs the shape of the SDK never leaves the chrome; only a reader who needs a specific signature
does.

## Information Architecture

```text
/reference/
  index          one card per surface: its artifact, its generator, its drift gate
  api/           REST, from the Kynos OpenAPI document
  cli/           capsule(1) — command tree, man pages, completions
  sdk/
    rust/        capsule-sdk narrative; links into crates/
    swift/       uniffi namespaces: capsule_core, capsule_core_ffi, capsule_sdk
    kotlin/      the same namespaces, Kotlin bindings
    wasm/        the capsule-wasm browser surface
  crates/        workspace rustdoc (linked out)
```

The `Reference` sidebar group is hand-curated in `astro.config.mjs`, in the same style as `Design`
and for the same reason: generated pages must not be allowed to determine navigation order.

## Executable Examples, Not a Playground

The reflexive feature for an API reference is a try-it panel. For Capsule it would teach the wrong
model of the API.

The server is key-free. Every write is sealed on the client before it is sent, and the sync feed
returns opaque envelopes. An HTTP-level playground could exercise the version handshake, the auth
flows, and blob fetch; everything that distinguishes Capsule from a file server would either return
ciphertext or reject an unsealed body. A reader who succeeded at such a playground would leave
believing the API accepts plaintext.

The honest equivalent is the command line, because `capsule` already performs the sealing. Examples
therefore belong where they can actually be executed: doctests in the SDK, and command transcripts in
the CLI reference. This is a reason to build a different feature, not a reason to build nothing.

## Deferred

- **Per-endpoint lifecycle metadata in the spec** — stability, required permission, and a per-version
  changelog carried as vendor extensions so badges and endpoint histories render straight from the
  contract. Considered and not adopted now; revisit once a Kynos document exists and more than one
  generation of client has to stay compatible.
- **Docs versioning.** `starlight-versions` is installed and left commented out. A version switcher
  is meaningless before the REST contract stabilizes.
- **Reference internationalization.** The site has no locale routing yet
  ([Internationalization](/design/i18n/) owns the catalog contract); generated reference is the last
  content that should acquire one.
- **Deploy automation.** Publishing is still a manual `bun run deploy`, and hosting rustdoc beside
  the site needs it automated first.

## Validation

Tier: **docs build**, plus one drift gate per surface.

- `mise run check-docs` formats, lints, and builds the site. The link validator fails the build on a
  dead cross-reference, which is what keeps the overview pages honest as generated routes appear and
  move.
- Each artifact's `--check` runs in its owning toolchain gate. Nothing Rust-shaped runs in the docs
  gate.
- The docs job's CI path filter is itself part of the contract: it must name every artifact the docs
  build reads. Reviewing a new reference surface means reviewing that filter.
