---
title: Licensing
description: Capsule's outbound licence, the inbound dependency-licence policy, and the linkage rules that keep both enforceable
status: draft
---

This doc owns Capsule's licensing position: the outbound licence, the licences a dependency may
carry, and the linkage rules. Admitting a licence not already permitted requires editing this doc
first; `deny.toml` is the machine check. Which library implements a concern is
[Dependencies](/design/dependencies/); the shipped attribution text is the repo-root `NOTICE`.

## Outbound: what Capsule is published under

Capsule is **`AGPL-3.0-only`** (never `-or-later`), copyright held solely by Justin Chung. One
`license` in the root `[workspace.package]`, inherited by every member and mirrored in
`package.json`, `pyproject.toml` and the OpenAPI document.

### Dual licensing is the mechanism, not an afterthought

App-store terms impose restrictions the AGPL forbids, so an App Store build can only be a separate
grant from the sole copyright holder. That right exists only while Capsule owns 100% of the
copyright, which is what [`CLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CLA.md)
(§4, relicensing) preserves. **Every contribution MUST arrive under the CLA.** Released versions stay
AGPL irrevocably.

## Inbound: what Capsule will accept from a dependency

### Permitted

Attribution-only terms, discharged by `NOTICE`: `MIT`, `MIT-0`, `Apache-2.0`, `BSD-2-Clause`,
`BSD-3-Clause`, `ISC`, `Zlib`, `BSL-1.0`, `Unicode-3.0`, `CDLA-Permissive-2.0`, `CC0-1.0`, `0BSD`,
`Unlicense`, and `MPL-2.0`.

`MPL-2.0` is file-level copyleft and Capsule links MPL crates unmodified; modifying an MPL file
in-tree requires revisiting this doc. For a multi-licensed crate Capsule **elects a permitted arm**
and records it in `NOTICE`; a disjunctive licence with a forbidden arm is fine (e.g. `r-efi`, elected
MIT), a conjunctive one is not.

### Permitted only as a named exception

A bounded, dischargeable obligation beyond attribution needs a per-crate `deny.toml` exception and a
`NOTICE` entry:

| Crate | Licence | Why it is bounded |
| --- | --- | --- |
| `tzf-rel` | `ODbL-1.0` | Share-alike on the database only; embedded unmodified, no derivative database. |
| `jpeg-encoder` | `(MIT OR Apache-2.0) AND IJG` | IJG requires only a fixed notice. |
| `libfuzzer-sys` | `(MIT OR Apache-2.0) AND NCSA` | Attribution-only; reached only via rav1e's `cfg(fuzzing)`, so no artefact compiles it. |

### Forbidden

**`GPL-*`, `LGPL-*` and `AGPL-*` MUST NOT enter the dependency tree**; `deny.toml` denies them. They
are licence-compatible with an AGPL project, but would let a third party foreclose the app-store
path by merging a dependency bump.

## Linkage rules

1. **No statically linked copyleft.** The CLI, the iOS staticlib and the wasm bundle are static
   artifacts.
2. **LGPL is not a safe harbour on iOS**: a signed App Store binary cannot be relinked. Any future
   LGPL exception must be off for app-store targets, dynamically linked elsewhere, and carry a
   relinking offer in `NOTICE`.
3. **Native components are optional features, absent from `default`.**
4. **Build-time tooling is out of scope** while it is never shipped: `sharp`/libvips (LGPL) builds
   the docs site; shipping an image containing `node_modules` is a licensing change.

### The video transcoder seam

The planned `capsule-core::media::video` transcoder seam
([Thumbnails — Video Previews](/design/thumbnails/#video-previews)) is the likeliest route for
copyleft, so its constraints are fixed now: prefer the platform framework (AVFoundation,
MediaCodec); ffmpeg only as an LGPL-2.1 base build (never `--enable-gpl`, `libx264`, `libx265`),
never statically linked, never in an app-store target. Adding ffmpeg requires editing this doc and
`deny.toml` first.

## Enforcement

`deny.toml` (run by `mise run license-check` in `check-rust` and CI) is the only gate that fails a
build. `NOTICE` ships with every release artifact; chain of title is
[`CLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CLA.md) and
[`CCLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CCLA.md).

### Known gaps

- **No CLA signature record** (PR checkbox only); a CLA bot is a prerequisite for outside
  contributions.
- **No AGPL §13 source link** in `capsule-web`; self-hosters carry that obligation.
- **No SBOM**; `NOTICE`'s third-party section is maintained by hand.
