---
title: Licensing
description: Capsule's outbound licence, the inbound dependency-licence policy, and the linkage rules that keep both enforceable
audit: none
---

This doc owns Capsule's licensing position. Admitting a licence not already permitted requires
editing this doc first; `deny.toml` is the machine check. The reason a library implements a concern
is a comment beside its pin, and the shipped attribution text is the repo-root `NOTICE`.

## Outbound

- **`AGPL-3.0-only`**, never `-or-later`. One `license` in the root `[workspace.package]`, inherited
  by every member and mirrored in `package.json`, `pyproject.toml` and the OpenAPI document.
- **Sole copyright holder:** Justin Chung.
- **Dual licensing for app stores.** App-store terms impose restrictions the AGPL forbids, so an
  App Store build is a separate grant from the sole copyright holder. That right exists only while
  Capsule owns 100% of the copyright, which
  [`CLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CLA.md) (§4, relicensing)
  preserves. **Every contribution MUST arrive under the CLA.** Released versions stay AGPL
  irrevocably.

## Inbound

**Permitted** (attribution only, discharged by `NOTICE`): `MIT`, `MIT-0`, `Apache-2.0`,
`BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `BSL-1.0`, `Unicode-3.0`, `CDLA-Permissive-2.0`,
`CC0-1.0`, `0BSD`, `Unlicense` and `MPL-2.0`.

- **Multi-licence election.** For a multi-licensed crate Capsule **elects a permitted arm** and
  records it in `NOTICE`. A disjunctive licence with a forbidden arm is fine (e.g. `r-efi`, elected
  MIT); a conjunctive one is not.
- **MPL is file-level.** `MPL-2.0` is file-level copyleft and Capsule links MPL crates unmodified.
  Modifying an MPL file in-tree requires revisiting this doc.

### Permitted only as a named exception

A bounded, dischargeable obligation beyond attribution needs a per-crate `deny.toml` exception and
a `NOTICE` entry.

| Crate | Licence | Why it is bounded |
| --- | --- | --- |
| `tzf-dist` | `ODbL-1.0` | Share-alike binds the database only; embedded unmodified, no derivative database. |
| Geist, Geist Mono | `OFL-1.1` | Font files the docs site vendors unmodified; the licence binds the fonts, never the pages using them. Not a crate, so outside `deny.toml`. |

### Forbidden

**`GPL-*`, `LGPL-*` and `AGPL-*` MUST NOT enter the dependency tree**, and `deny.toml` denies them.
They are licence-compatible with an AGPL project, but a merged dependency bump would let a third
party foreclose the app-store path.

## Linkage

1. **No statically linked copyleft.** The CLI, the iOS staticlib and the wasm bundle are static
   artifacts.
2. **LGPL is not a safe harbour on iOS:** a signed App Store binary cannot be relinked. Any future
   LGPL exception must be off for app-store targets, dynamically linked elsewhere, and carry a
   relinking offer in `NOTICE`.
3. **Native components are optional features, absent from `default`.**
4. **Build-time tooling is out of scope while it is never shipped.** `sharp`/libvips (LGPL) builds
   the docs site; shipping an image containing `node_modules` is a licensing change.

### The video transcoder seam

The planned `capsule-core::media::video` seam ([](/design/thumbnails/#video-previews)) is the
likeliest route for copyleft, so its toolchain is decided before any of it is written.

- **Video decode and transcode come from `rawshift-video`**, reached through `capsule-core::media`
  like stills. Capsule links no demuxer, video decoder or encoder of its own. Rawshift keeps
  software H.264 and HEVC out on patent grounds, so encode goes through platform hardware codecs
  (upstream names VAAPI first) behind its `VideoEncoder` seam.
- **ffmpeg is excluded from every build.** Its only permitted arm is a dynamically linked LGPL-2.1
  base build without `libx264` or `libx265`. Rule 2 rules that out for the phone apps, which come
  first, and a desktop-only exception would duplicate Rawshift's work. `deny.toml`'s `[bans]`
  refuses the ffmpeg, x264 and x265 binding crates by name, so a binding under a permissive crate
  licence is refused too.

Lifting either rule requires editing this doc and `deny.toml` first.

## Model Weights and Datasets

ML weights are data that clients fetch at runtime ([](/design/ai/#distribution-and-readiness)).
They are never linked, bundled into an app-store binary or server image, or listed in `NOTICE`.
Their licences are still gated, because a released weight is something Capsule publishes and a user
runs. Each release's weight licence and training-data licences are recorded in its registry record
([](/design/ai/#weight-registry)), and this section decides which ones are admissible.

- **Permitted:** the inbound permitted list above, plus open-data licences that require only
  attribution (e.g. `CC-BY-4.0`, `CDLA-Permissive-2.0`, `ODC-By-1.0`).
- **Forbidden:** non-commercial (`*-NC-*`, research-only), no-derivatives, and use-restricted model
  licences, for both weights and the datasets they were trained on. This applies to every released
  lineage, whether or not a given build is commercial, because the app-store grant
  ([](#outbound)) must stay available.
- **Hard copyleft** (`GPL-*`, `AGPL-*`, `CC-BY-SA-*`, `ODbL-1.0` and the like) is forbidden unless the
  closest permitted alternative is **much worse** on the release's own evaluation. Such an exception
  is recorded in the registry record with that comparison, and it is re-examined at every later
  release of the lineage.
- **Training in-project** on permitted data is the remedy when nothing admissible exists. The
  weights it produces are released under a permitted licence.
- No weight or dataset counts as chosen until its registry record carries a verified licence and an
  evaluation. A candidate named in a design doc is not admitted by being named.

## Enforcement and Known Gaps

`deny.toml`, run by `mise run license-check` in `check-rust` and CI, is the only gate that fails a
build. `NOTICE` ships with every release artifact. Chain of title is
[`CLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CLA.md) and
[`CCLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CCLA.md).

- **No CLA signature record** (PR checkbox only). A CLA bot is a prerequisite for outside
  contributions.
- **No AGPL §13 source link** in `capsule-web`; self-hosters carry that obligation.
- **No SBOM.** `NOTICE`'s third-party section is maintained by hand.
- **Weight and dataset licences are checked by review only.** No gate reads the weight registry
  yet.
