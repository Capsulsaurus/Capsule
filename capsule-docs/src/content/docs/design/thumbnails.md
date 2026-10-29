---
title: Thumbnails and Previews
description: Format inventory, LQIP scheme, and derivative provenance for photo and video derivatives
status: draft
---

This doc is the single source of truth for the LQIP scheme and the derivative formats; other docs link here rather than restating them. The format table is a contract: a receiver (including a federated peer) MUST reject a `DerivativeManifest.format` value not listed here.

Rawshift owns decoding, metadata extraction and derivative generation (consumed through `capsule-core::media`); the server stores only opaque ciphertext. The [LQIP](#lqip) is the exception: Capsule computes it with Chromahash directly in `capsule-core::lqip`.

## Media Is Rawshift's

Capsule embraces every format and capability its pinned [Rawshift](https://github.com/visualcommons/rawshift)
release supports, and implements none of them itself. Decoding, RAW development, video, resizing,
colour management, EXIF/XMP/ICC extraction and hardware-accelerated encode and decode are
Rawshift's; a gap is fixed upstream, not worked around in Capsule.

`capsule-core::media` is a thin adapter that owns only what is Capsule's: the pixel budget and
decoder sandbox (`catch_unwind`), the [tier table](#thumbnail-and-preview-formats), metadata
stripping before encryption, and [derivative signing](#derivative-provenance). Its tests are
lean integration tests over a fixture corpus (one file per supported format and per known-hostile
shape), not re-tests of Rawshift.

Code that duplicates Rawshift today and is deleted once the Rawshift release it waits for is
pinned: Capsule's own format sniffing, the TIFF/RAW IFD pre-walk, EXIF parsing through
`kamadak-exif`, the AV1 container pre-check, and the integer resize.

**Every file is kept, whatever Rawshift supports.** An original in a format no codec in the build
handles, video included, is still imported, encrypted, uploaded, verified and restorable byte for
byte; only its derivatives are absent, recorded as deferred and generated when a later build can.
Support for a format governs previews, never whether Capsule will hold the file.

## Thumbnail and Preview Formats

**JXL is the primary** still codec (the master derivative). Every still tier is also delivered as **AVIF**, with **WebP** as last resort, so a client without a JXL decoder still renders. There is no JPEG fallback.

:::note[JXL-primary is provisional]
Pending validation of decoder coverage and quality-per-byte in the [image-delivery-format demo](https://github.com/justin13888/image-delivery-format-demo). If it fails, the primary reverts to AVIF — a one-row edit here.
:::

| Tier | Photo format | Video format | Notes |
| --- | --- | --- | --- |
| **Thumbnail** (grid) | JXL master; AVIF→WebP delivery | First-frame JXL/AVIF still | q=50, 4:2:0, ~256 px long edge |
| **Preview** (single-asset view) | JXL master; AVIF→WebP delivery | H.264 baseline, capped to 1080p | Stills q=70; H.264 CRF 23, 30 fps cap, AAC |

H.264 baseline is chosen for universal cheap decode; AV1 mobile encode is still too costly.

:::note[What the build generates today]
The table is the contract, not an inventory. Native encoders sit behind the opt-in `media-linked-codecs` feature (CLI only); the mobile FFI and cross targets stay pure Rust.

| Build | Thumbnail | Preview | Missing |
| --- | --- | --- | --- |
| `media-linked-codecs` (CLI) | JXL (lossy q=50) + AVIF | JXL + AVIF, q=70 | WebP (`rawshift-image` fails to compile it on aarch64) |
| `media` only (FFI, cross) | JXL **lossless** (q=50 is advisory) | none | AVIF, lossy JXL (native code), WebP |
| Either | — | — | All video derivatives (`rawshift-video` unpublished) |

Decode covers JPEG, PNG, JXL, TIFF, GIF, ARW and DNG in every build, plus AVIF under `media-avif-decode` (system libdav1d; not on Windows). Capsule pre-walks RAW IFD chains to bound value sizes and nesting depth (≤16) before handing them to the RAW reader. **HEIC is refused by decision** (libheif/libde265 are LGPL-3.0; see [Dependencies](/design/dependencies/)); WebP, other RAW families, and AVIF without the feature are recognised and refused.

Nothing is silent: an absent codec is `media::MediaError::UnsupportedFormat`, and an unencodable `(tier, format)` pair is recorded as deferred and counted in the import summary while the asset's other derivatives still ship.
:::

### Video Previews

Video derivatives (first-frame still, H.264 preview) are the planned `capsule-core::media::video` half of the media module, separate because transcode needs its own toolchain. They are signed through the same [`DerivativeManifest`](#derivative-provenance) path as stills.

If an original is no larger than a tier's long edge, that tier references the original: its local manifest carries the sentinel `format = "original"`, which is distinct from a missing derivative (rebuildable from the original).

### Embedded Previews

Camera RAW files usually carry a camera-rendered JPEG inside them, anywhere from thumbnail size to near full resolution depending on the camera and format. Rawshift extracts it without developing the image (`RawFile::thumbnail()`), for every RAW format the build decodes. A device holding the original can therefore show a large view at once, before any derivative is encoded. Its size decides which rung it can fill: the extracted JPEG is whichever one the format designates, which is not always the largest in the file.

- **Local only.** An embedded preview is read from the original on the device that holds it and is never uploaded, signed or listed as a tier. It is not a `DerivativeManifest.format`, so the no-JPEG rule for delivered tiers is unaffected. Other devices get the encoded [tiers](#thumbnail-and-preview-formats).
- **A stand-in, not a replacement.** On the [render ladder](/design/transfer/download/#render-ladder) it fills each rung it is large enough for, until that rung's encoded tier exists. It fills Thumbnail when its long edge reaches the Thumbnail tier's. It fills Preview when it covers the surface showing it, in device pixels, because the Preview tier is kept at source resolution and an embedded JPEG rarely matches that. Import still generates the derivatives. It is the camera's rendering, not Capsule's, so an encoded Preview replaces it once one is available.
- **Bounded like any decode.** It goes through the same pixel budget and sandbox as every other input. An embedded preview that is absent or does not decode leaves the rung to the next source.

## LQIP

Capsule imports [Chromahash](https://github.com/visualcommons/chromahash) **0.7.1** directly, never through Rawshift; the pin lives in [Dependencies](/design/dependencies/#rust). ThumbHash is retired; see ADR-0003.

The chromahash, its format version and a `dominant_color` fallback form the [`lqip` sidecar field](/design/metadata/#sidecar-schema-v1), inside the [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption): the placeholder arrives with metadata sync, before any thumbnail, and never reaches the server. A decoder that does not recognise the format version, or bytes `from_bytes` rejects, MUST fall back to the solid `dominant_color` fill. Changing the encoding requires a new format version — `lqip` is signed, and byte length alone cannot discriminate encodings.

### Encoding Tier

Always Chromahash's `DEFAULT_TIER` — **exactly 32 bytes** — never varied per asset, so the signed sidecar's size is predictable. Rendering uses `decode_capped` at the painted box size; the fill uses `average_color()`.

### Where LQIP Lives

`capsule-core::lqip`, not `capsule-core::media`: `media` is native-only, while the LQIP must reach the import pipeline, the uniffi FFI and the browser via `capsule-wasm` from one implementation.

## Derivative Provenance

Derivatives are regenerable but not unowned: adding or replacing one is a signed lifecycle action, so a buggy or hostile client cannot silently swap a thumbnail. The manifest structure and the `derivative-add` / `derivative-replace` actions are owned by [Cryptography — Derivative Provenance](/design/cryptography/provenance/#derivative-provenance) and [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set); this doc owns only the `format` values, checked under the [closed-enum rule](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar).

- A receiver MUST NOT schedule regeneration for a tier the sidecar's `dimensions` show the original already satisfies; the `original` sentinel is local and never shipped.
- A derivative whose manifest fails verification is discarded (not quarantined) and regenerated from the original, appending a new `derivative-replace` record.

## Validation

- **Format detection / closed enum (unit).** Each table row is identified correctly; malformed AVIF and `format = "image/future-codec"` are rejected.
- **Delivery fallback (unit).** No JXL decoder → AVIF; no AVIF → WebP.
- **LQIP (unit).** Payload is 32 bytes; `decode_capped` honours the bound; unknown version or invalid bytes → `dominant_color`.
- **Manifest verification (smoke).** Corrupted derivative is discarded, regenerated, and a `derivative-replace` record appended.
- **Original fallback (unit).** A small original yields `format = "original"`, not a redundant derivative.

The end-to-end case is in [Module Map](/design/module-map/#e2e-test-surface).
