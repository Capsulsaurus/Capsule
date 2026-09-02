---
title: Licensing
description: Capsule's outbound licence, the inbound dependency-licence policy, and the linkage rules that keep both enforceable
status: draft
---

This doc is the single owner of **Capsule's licensing position**: the terms Capsule is published under, the terms it will accept from a dependency, and the linking rules that keep a permissively-licensed binary permissively licensed. Adding a dependency whose licence is not already permitted requires editing this doc first, and `deny.toml` is the machine check that the tree matches what is written here.

What this doc deliberately does **not** own, per the [SSoT rule](/design/principles/#single-source-of-truth):

- **Which library implements which concern** — [Dependencies](/design/dependencies/). That doc pins the *implementation*; this doc governs the *licence* that implementation may carry.
- **The attribution text Capsule actually ships** — the repo-root `NOTICE`, which is a legal artifact rather than a design document. This doc says what must be attributed; `NOTICE` is where it is attributed.

## Outbound: what Capsule is published under

Capsule is published under **`AGPL-3.0-only`**. Copyright is held solely by Justin Chung.

`-only`, never `-or-later`. "Or later" would let a future FSF revision restate Capsule's own outbound grant, and a project whose licensing position depends on the copyright holder controlling its terms cannot delegate that. Every manifest declares the same string — one `license` in the root `[workspace.package]`, inherited by members with `license.workspace = true`, mirrored in `package.json`, `pyproject.toml`, and the OpenAPI document.

### Dual licensing is the mechanism, not an afterthought

The AGPL is the licence Capsule is *published* under. It is not the only licence the copyright holder may offer it under, and the difference is load-bearing:

**App stores cannot accept AGPL software.** Apple's and Google's terms of service impose usage restrictions — device limits, no redistribution, DRM — that the AGPL's "no further restrictions" clause forbids. A shipped Mac App Store or iOS build therefore cannot be an AGPL distribution. It can only be a separate grant from the copyright holder to the store and its users.

This is legal for Capsule and was not legal for VLC, and the difference is entirely chain of title. VLC was pulled from the App Store in 2011 because a *contributor* asserted the GPL against the distribution. A sole copyright holder is not bound by their own outbound licence and is the only party with standing to object. Capsule's ability to ship to an app store is therefore a direct function of owning 100% of the copyright — which is what [`CLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CLA.md) exists to preserve, and why its §4 states the relicensing right explicitly rather than leaving it implied by the sublicensing grant.

Publishing under the AGPL is irrevocable for versions already released: anyone who received a release keeps their AGPL rights to it permanently, and may fork it. That is the accepted cost, not a defect.

**Consequence for contributions:** every contribution must arrive under the CLA. A single contribution whose copyright Capsule does not control removes the ability to ship that build to an app store at all.

## Inbound: what Capsule will accept from a dependency

A dependency's licence must not constrain how a Capsule binary may be distributed. Licences fall into three tiers.

### Permitted

Attribution-only and notice-only terms. These impose no obligation beyond shipping their text, which the repo-root `NOTICE` does.

`MIT`, `MIT-0`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `BSL-1.0`, `Unicode-3.0`, `CDLA-Permissive-2.0`, `CC0-1.0`, `0BSD`, `Unlicense`, and `MPL-2.0`.

`MPL-2.0` is included deliberately. It is a file-level copyleft: the obligation attaches to the MPL-covered files themselves, not to anything that links them, and §3.3 expressly permits distributing a larger work under other terms. Capsule links MPL crates unmodified, so the obligation is discharged by pointing at their published source. Modifying an MPL-covered file in-tree would change this and requires revisiting this doc.

Where a dependency offers a choice of licences, Capsule **elects a permitted term** and records the election in `NOTICE`. A disjunctive licence containing a forbidden arm is acceptable; a conjunctive one is not.

### Permitted only as a named exception

A licence that imposes an obligation beyond attribution, where that obligation is bounded and dischargeable, may be admitted as a per-crate exception in `deny.toml` with a matching `NOTICE` entry. Two exist:

| Crate | Licence | Obligation and why it is bounded |
| --- | --- | --- |
| `tzf-rel` | `ODbL-1.0` | Share-alike on the *database*, not on code that queries it. Capsule embeds the timezone shapefile unmodified and creates no Derivative Database, so the obligation is attribution plus pointing at the unmodified upstream. |
| `jpeg-encoder` | `(MIT OR Apache-2.0) AND IJG` | The IJG arm is conjunctive and cannot be elected away, but it requires only a fixed notice. |

An exception is a decision, not a waiver. Adding one means writing down what the obligation is and how Capsule discharges it.

### Forbidden

**`GPL-*`, `LGPL-*`, and `AGPL-*` may not enter the dependency tree.** `deny.toml` denies them outright.

This is stricter than the law requires, and deliberately so. Capsule is itself AGPL, so a GPL-family dependency is licence-*compatible* today; the reason to refuse it is that it would silently convert the App Store path from "a decision the copyright holder makes" into "a decision a third party already made". The point of the gate is that no one can take that option away by merging a dependency bump.

A disjunctive licence offering a permitted arm alongside a copyleft one is fine — Capsule elects the permitted arm. `r-efi` (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`) is the live example: Capsule elects MIT.

## Linkage rules

Licence identity alone does not settle whether a binary is distributable. How the code is linked decides it.

1. **Static linking of any copyleft component is forbidden.** Static linking is what pulls a dependency's terms into the resulting binary. The `capsule` CLI, the iOS staticlib, and the wasm bundle are all statically linked artifacts, so anything reaching them carries no copyleft under the rules above.

2. **LGPL is not a safe harbour on iOS.** LGPL §4 permits proprietary linking only if the user can relink the work against a modified version of the library. On a code-signed App Store binary the user cannot relink or re-sign, so an LGPL component in an iOS build is non-compliant *however* it is linked. If an LGPL exception is ever admitted, it must be feature-gated off for every app-store target, dynamically linked everywhere else, and recorded in `NOTICE` with a relinking offer.

3. **Native components must be feature-gated and default-off.** Any dependency pulling a C/C++ toolchain declares an optional feature that is absent from `default`. This keeps a demonstrable minimal build and keeps the licence surface of a release build auditable.

4. **Build-time tooling is out of scope.** A licence obligation attaches on distribution. `sharp`/libvips (`LGPL-3.0-or-later`) is a build-time image optimizer for the docs site; it is dynamically loaded, never packaged, and no container image ships `node_modules`. It is exempt for exactly as long as that stays true — **shipping a deploy image containing `node_modules` would attach the LGPL notice and relink obligations** and must be treated as a licensing change.

### The video transcoder seam

`capsule-core::media::video::derivative` **will** define a `VideoTranscoder` seam with no implementation — `capsule-core::media` is on `capsule-docs/planned-modules.txt` and the seam retired to `legacy-review/media-pipeline/` with the rest of the stack in slice `S-C59`, so this section is a constraint on the rebuild rather than a description of the tree. Core links no media toolchain, and `capsule-sdk` injects a per-platform one — ffmpeg, AVFoundation, or MediaCodec — per [Thumbnails — Video Previews](/design/thumbnails/#video-previews) and slice `S-B5`. It is the most likely route by which copyleft enters Capsule, so the constraint is written before the code:

- **Prefer the platform framework.** AVFoundation (Apple) and MediaCodec (Android) are OS-provided, carry no third-party licence, and are the sanctioned implementations on those platforms.
- **ffmpeg, if used, must be an LGPL-2.1 base build.** Never `--enable-gpl`. Never `libx264` or `libx265` — both are GPL, and enabling either makes the whole ffmpeg binary GPL.
- **Never statically linked**, per rule 1, and never present in an app-store target, per rule 2.
- ffmpeg is not a normal dependency bump. It is a licensing change, and it requires editing this doc and `deny.toml` first.

## Enforcement

| Layer | Mechanism |
| --- | --- |
| Outbound declaration | `license` in `[workspace.package]`, inherited by every member; mirrored in `package.json`, `pyproject.toml`, and the OpenAPI document |
| Inbound allowlist | `deny.toml`, run by `mise run license-check` as part of `check-rust` and in CI |
| Attribution | repo-root `NOTICE`, shipped with every release artifact by `release.yml` |
| Chain of title | [`CLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CLA.md) and [`CCLA.md`](https://github.com/Capsulsaurus/Capsule/blob/master/CCLA.md) |

`deny.toml` is the only one of these that fails a build. The others are conventions this doc exists to record.

### Known gaps

- **No CLA signature record.** Agreement is a pull-request checkbox; there is no bot and no registry. Adequate while the copyright holder is the sole contributor, and not adequate afterwards — installing [CLA Assistant](https://github.com/cla-assistant/cla-assistant) is a prerequisite for accepting outside contributions.
- **AGPL §13 source offer.** §13 requires a network-interactive AGPL deployment to offer its source to remote users. `capsule-web` has no such link. This is an obligation on anyone who self-hosts Capsule, including the project.
- **No SBOM.** `deny.toml` gates licences but nothing emits a bill of materials. `cargo-about` over the same metadata would generate `NOTICE`'s third-party section rather than maintaining it by hand.
