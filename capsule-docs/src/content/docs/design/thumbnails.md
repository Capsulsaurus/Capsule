---
title: Thumbnails and Previews
description: Format inventory, LQIP scheme, and derivative provenance for photo and video derivatives
status: draft
---

We generate thumbnails and previews for all photos and videos. This doc is the **single source of truth** for the LQIP scheme and the thumbnail/preview formats — per the [SSoT rule](/design/principles/#single-source-of-truth), other docs reference these by link rather than restating the choice. The format table is itself the contract: every receiver (and every federated peer) compares the `DerivativeManifest.format` value against this list, and an unknown value is a structural rejection.

Rawshift owns client-side media decoding, metadata extraction, and derivative generation. Capsule core maps those outputs into signed manifests, and the planned `capsule-api::blob` module serves only opaque ciphertext. The [LQIP](#lqip) is the one derivative Rawshift does **not** own: Capsule imports Chromahash directly and computes it in `capsule-core::lqip`.

## Thumbnail and Preview Formats

**JPEG XL (JXL) is the committed primary** still-image codec — the highest-quality-per-byte master derivative. Because JXL decoder coverage is still uneven in 2026, every still tier is *also* generated in **AVIF** (with **WebP** as the last-resort fallback): a client that can decode JXL fetches it, and any other client is served the AVIF→WebP delivery variant and still renders. Because this doc is the SSoT, the codec choice is a one-row edit here that propagates nowhere else (see [SSoT](/design/principles/#single-source-of-truth)).

:::note[JXL-primary is provisional]
The JXL-primary commitment is pending external validation of decoder availability and quality-per-byte across target devices — tracked in the [image-delivery-format demo](https://github.com/justin13888/image-delivery-format-demo). If that validation shows JXL coverage is insufficient, the primary reverts to AVIF — a one-row edit here that propagates nowhere else.
:::
<!-- TODO: resolve the JXL-primary commitment after external validation — https://github.com/justin13888/image-delivery-format-demo -->

Two derivative tiers per photo asset and one preview tier for video assets:

| Tier                                       | Photo format                               | Video format                                                        | Notes                                              |
| ------------------------------------------ | ------------------------------------------ | ------------------------------------------------------------------- | -------------------------------------------------- |
| **Thumbnail** (grid display)               | **JXL** master; **AVIF**→**WebP** delivery | First-frame JXL/AVIF still                                          | q=50, 4:2:0 chroma, ~256 px long edge.             |
| **Preview** (lightbox / single-asset view) | **JXL** master; **AVIF**→**WebP** delivery | **H.264 baseline** transcode at original resolution capped to 1080p | Stills q=70; H.264 CRF 23, 30 fps cap, AAC audio.  |

- **JXL** is the committed primary: best quality-per-byte and an excellent archival master. Its only gap is decoder ubiquity, which the AVIF/WebP delivery variants cover until JXL coverage is universal.
- **AVIF** is the universal delivery format — in 2026 it ships in every major browser and OS (iOS 16+, Android 12+, current Chrome/Firefox/Safari) with widespread hardware decode — served to any client that cannot yet decode JXL.
- **WebP** is the last-resort fallback for the rare client lacking AVIF. We deliberately do not fall back to JPEG — WebP covers everything JPEG would.
- **H.264 baseline** for video previews — universally decodable, cheap to decode on every platform. AV1 was considered but mobile encode cost is still high in 2026.

### Video Previews

The table above stays the SSoT for the video formats; this section only names the implementation seam. Video derivative generation — the first-frame still and the H.264 baseline preview transcode — is its own implementation slice (`S-B5` in the repo-root `SLICES.md`), split from still-image generation (`S-B1`) because transcode brings a distinct toolchain (demux, video decode, H.264/AAC encode) the still path never touches. Both slices sign their outputs identically through the [`DerivativeManifest`](#derivative-provenance) path.

If an original asset is lower-resolution than the highest thumbnail tier, that tier references the original instead of generating a redundant derivative. This is **distinct** from a missing derivative (an unintentional generation failure): the tier's [`DerivativeManifest`](/design/cryptography/provenance/#derivative-provenance) carries the recognized sentinel `format = "original"` — an explicit, signed marker the receiver trusts — whereas a simply-absent derivative is treated as rebuildable from the original (recovery-first).

## LQIP

Capsule imports [Chromahash](https://github.com/justin13888/chromahash) **0.7.1** directly; Rawshift does not wrap or expose it. The earlier gate — "after Chromahash reaches v1" — is **amended to 0.7.1**, the release the project accepts as ready, and the architecture check stopped forbidding the crate accordingly (`2f8beeb`). The crate pin lives in [Dependencies](/design/dependencies/#rust); this section owns the contract that pin has to satisfy.

The chromahash, its format version, and a `dominant_color` fallback are the [`lqip` field of the sidecar](/design/metadata/#sidecar-schema-v1) — inside the [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption), so the placeholder is available the instant metadata syncs, before any thumbnail fetch, and never leaks to the server. A decoder that does not recognize the chromahash format version falls back to the solid `dominant_color` fill rather than misrendering, so a future chromahash revision is a versioned change, never a silent break.

### Encoding Tier

**`DEFAULT_TIER` — exactly 32 bytes**, which is what `ChromaHash::encode` produces. Chromahash's ladder runs tier 0–4 from `COMPACT_TIER` (21 bytes, lowest fidelity) upward; the default is the committed choice and Capsule does not vary it per asset. The sidecar is the one representation a client is guaranteed to hold — it is tiny, canonical, and effectively never evicted, unlike every heavier tier — so the eleven bytes bought over `COMPACT_TIER` are spent exactly where fidelity always survives, and a fixed width keeps the signed sidecar's encoded size predictable.

Four calls carry the whole contract, and the module uses no more than these:

| Call | Role |
| --- | --- |
| `encode(width, height, &rgba, gamut)` | Generation at `DEFAULT_TIER`. `encode_with_quality(.., tier)` exists; Capsule does not reach for it. |
| `decode_capped(max_w, max_h)` | The band-limited render — decode no larger than the box actually being painted, so a grid cell never scales down a full-size decode. |
| `average_color()` | The DC-only path: the `dominant_color` fill without a full decode. |
| `from_bytes` / `as_bytes` | The sidecar round trip. `from_bytes` is fallible, so bytes that are not a valid chromahash become the fallback fill rather than a render. |

### Where LQIP Lives

`capsule-core::lqip` — a dedicated module, slice `S-B14` in the repo-root `SLICES.md`. It is deliberately **not** in `capsule-core::media`, which retires to `legacy-review/` with the rest of the decode/encode stack: a placeholder scheme every client depends on cannot live inside something scheduled for teardown. It is equally not in Rawshift — `AGENTS.md` is explicit that Rawshift owns media decoding but must not wrap Chromahash, which Capsule imports directly.

A small Capsule-owned module outside the retiring stack satisfies both constraints at once, and is reachable from all three places a placeholder is produced or consumed: the import pipeline, the native apps through the uniffi FFI, and the browser through `capsule-wasm`. That is the point of a single home — one implementation for every surface, so a photo's placeholder does not depend on which client happened to import it.

### Migrating off ThumbHash

ThumbHash was the interim implementation while Chromahash was unreleased, and it retires with this decision — both the `thumbhash` Rust crate behind `capsule-core`'s `media` feature and the npm `thumbhash` package `capsule-web` still decodes in `lazy-image.tsx` (fed from `asset-grid.tsx`).

`lqip` is a **signed** sidecar field. The signature covers it, so changing its encoding is signature-visible rather than a private implementation detail, and the migration is only free if nothing already committed depends on the old bytes. Two facts make it free here:

- **Nothing pins a ThumbHash payload.** No fixture, golden, or known-answer vector carries one: the committed KATs are drop and share vectors that never touch a sidecar, and the only `lqip` literal in the tree is a synthetic eight-byte filler in a round-trip unit test.
- **The schema always named chromahash.** The field is `Lqip.chromahash` and format version 1 is declared as the *chromahash* format version. ThumbHash bytes were standing in for an unreleased dependency; they were never the declared encoding.

So the format version stays `1` and `sidecar_schema` does not move. What makes that legitimate is that the migration is **total** — no persisted sidecar carries a ThumbHash payload under version 1 — and it must stay total, because ThumbHash payloads are shorter than 32 bytes and overlap the lower chromahash tiers in length, so byte length alone would not discriminate a stale one. Were such a sidecar ever to exist, the fix is a *new* format version, never a redefinition of this one: the versioned fallback above is precisely the mechanism that makes a re-encoding a legal, non-breaking change instead of a silent misrender.

Considered and rejected: ThumbHash on its merits (smaller wire size, worse color fidelity for the wide-gamut and HDR sources Capsule expects), BlurHash (older, blurrier, less color-accurate). The single-LQIP choice avoids exactly the kind of "chromahash/ThumbHash" hedge that previously caused doc drift.

## Derivative Provenance

Thumbnails and previews are *ephemeral by recovery posture* (they can always be regenerated from the original) but not *unowned*. A buggy or hostile client could otherwise quietly replace a good thumbnail with a corrupted one, and the receiving side would have no way to tell. To prevent this, every thumbnail and preview is uploaded as a derivative whose addition or replacement is an authorized, signed lifecycle action.

The full derivative manifest structure and the `derivative-add` / `derivative-replace` action set are owned by [Cryptography — Derivative Provenance](/design/cryptography/provenance/#derivative-provenance) and [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set); this doc owns only the *format* of the derivative bytes. The two interact at exactly one point: the `DerivativeManifest.format` field names the codec/format from the table above, and the verifying side rejects a manifest whose `format` is not currently recognized (the closed-enum rule from [Threat Model — Schema Rules](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).

A thumbnail whose `DerivativeManifest` fails verification is **regenerated locally from the original** rather than trusted — the [recovery-first principle](/design/principles/) means a derivative is always rebuildable, so refusal-and-regenerate is the safe default. The corrupt copy is discarded (not quarantined — it carries no irreplaceable bytes), and the corresponding regeneration appends a new `derivative-replace` provenance record.

## Validation

- **Format detection (unit).** Encode a derivative under each row of the format table; assert the format is correctly identified by the consumer (browser tier, native client tier). Negative: provide a malformed AVIF; assert structural rejection.
- **Closed-format enum (unit).** Submit a `DerivativeManifest` with `format = "image/future-codec"`; assert rejection at the envelope check.
- **JXL-to-AVIF delivery fallback (unit).** Simulate a consumer without a JXL decoder; assert it selects the AVIF variant (and a consumer without AVIF selects WebP), never failing to render a tier that exists.
- **LQIP round-trip (unit).** Generate chromahash for a fixture image; assert the payload is exactly 32 bytes (`DEFAULT_TIER`), that `decode_capped` matches the expected pixel buffer within quality tolerance at the requested bound, and that an unrecognized chromahash format version falls back to `dominant_color`.
- **No ThumbHash residue (unit).** Assert no sidecar fixture or vector carries a non-chromahash `lqip` payload, and that `from_bytes` rejecting a payload yields the `dominant_color` fill rather than a render.
- **Derivative-manifest verification (smoke).** Upload a derivative; corrupt the bytes; refetch; assert the receiver discards and regenerates from the original; assert a new `derivative-replace` provenance record is appended.
- **Original-fallback (unit).** Provide an original smaller than the highest thumbnail tier; assert that tier's manifest carries `format = "original"` rather than generating a redundant derivative.

The cross-module case — derivative generation → upload → fetch → display — is covered by the upload+sync E2E case in [Module Map](/design/module-map/#e2e-test-surface).
