---
title: Thumbnails and Previews
description: Format inventory, LQIP scheme, and derivative provenance for photo and video derivatives
audit: none
---

This page is the single source of truth for the LQIP scheme and the derivative formats; other docs link here rather than restate them. The format table is a contract: a receiver, including a federated peer, MUST reject a `DerivativeManifest.format` value not listed here.

Rawshift owns decoding, metadata extraction and derivative generation, consumed through `capsule-core::media`; the server stores only opaque ciphertext. The [](#lqip) is the exception: Capsule computes it with Chromahash directly in `capsule-core::lqip`.

## Media Is Rawshift's

Capsule embraces every format and capability its pinned Rawshift release supports and implements none of them itself. Decoding, RAW development, video, resizing, colour management, EXIF/XMP/ICC extraction and hardware-accelerated encode and decode are Rawshift's. A gap is fixed upstream, not worked around in Capsule.

`capsule-core::media` is a thin adapter that owns only what is Capsule's: the pixel budget and decoder sandbox (`catch_unwind`), the tier table ([](#thumbnail-and-preview-formats)), metadata stripping before encryption, and derivative signing ([](#derivative-provenance)). Its tests are lean integration tests over a fixture corpus (one file per supported format and per known-hostile shape), not re-tests of Rawshift.

Code that duplicates Rawshift today, deleted once the Rawshift revision it waits for is pinned: Capsule's own format sniffing, the TIFF/RAW IFD pre-walk, EXIF parsing through `kamadak-exif`, and the integer resize.

**Every file is kept, whatever Rawshift supports.** An original in a format no codec in the build handles, video included, is still imported, encrypted, uploaded, verified and restorable byte for byte. Only its derivatives are absent, recorded as deferred and generated when a later build can. Format support governs previews, never whether Capsule will hold the file.

## Thumbnail and Preview Formats

**JXL is the primary** still codec (the master derivative). Every still tier is also delivered as **AVIF**, with **WebP** as last resort, so a client without a JXL decoder still renders. There is no JPEG fallback.

:::note[JXL-primary is provisional]
Pending validation of decoder coverage and quality-per-byte in the [image-delivery-format demo](https://github.com/justin13888/image-delivery-format-demo). If it fails, the primary reverts to AVIF, a one-row edit here.
:::

| Tier | Photo format | Video format | Notes |
| --- | --- | --- | --- |
| **Thumbnail** (grid) | JXL master; AVIF→WebP delivery | First-frame JXL/AVIF still | q=50, 4:2:0, ~256 px long edge |
| **Preview** (single-asset view) | JXL master; AVIF→WebP delivery | H.264 baseline, capped to 1080p | Stills q=70; H.264 CRF 23, 30 fps cap, AAC |

H.264 baseline is chosen for universal cheap decode; AV1 mobile encode is still too costly.

If an original is no larger than a tier's long edge, that tier references the original: its local manifest carries the sentinel `format = "original"`, distinct from a missing derivative (rebuildable from the original).

:::note[What the build generates today]
The table is the contract, not an inventory. Native encoders sit behind the opt-in `media-linked-codecs` feature (CLI only); the mobile FFI and cross targets stay pure Rust.

| Build | Thumbnail | Preview | Missing |
| --- | --- | --- | --- |
| `media-linked-codecs` (CLI) | JXL (lossy q=50) + AVIF | JXL + AVIF, q=70 | WebP (not enabled) |
| `media` only (FFI, cross) | AVIF (q=50) | none | JXL (its only encoder, libjxl, is native code), WebP |
| Either | — | — | All video derivatives (`rawshift-video` is an unimplemented placeholder) |

Decode covers JPEG, PNG, JXL, TIFF, GIF, ARW and DNG in every build, plus AVIF under `media-avif-decode` (the CLI). Rawshift decodes an AVIF's AV1 codestream only through a hardware decoder (VAAPI on Linux today), so on a host without one, or whose decoder refuses the codestream's profile (VAAPI decodes AV1 Profile 0 only, not 4:4:4 Profile 1), an AVIF is deferred, and its header size is checked before decode and its decoded frame after it, not its coded frame; both gaps are filed upstream ([visualcommons/rawshift#78](https://github.com/visualcommons/rawshift/issues/78), [#79](https://github.com/visualcommons/rawshift/issues/79)). Capsule pre-walks RAW IFD chains to bound value sizes and nesting depth (≤16), the directories, values, strips and tiles a file states (twice its length each) and the SubIFD pointers it follows (≤256), a DNG's black-level pattern (≤8 a side) and bad-pixel lists, and a DNG's stated image and tile sizes, the pixels of every image IFD its eager reader decodes taken together, its `SamplesPerPixel` (≤4) and each lossless-JPEG chunk's own header size, before handing them to the RAW reader. The DNG bounds apply to any file whose IFD0 states `DNGVersion`, whatever its extension, because the reader opens every such file as a DNG ([visualcommons/rawshift#83](https://github.com/visualcommons/rawshift/issues/83)). A Deflate DNG is deferred, because the reader inflates it with no output bound ([visualcommons/rawshift#82](https://github.com/visualcommons/rawshift/issues/82)). **HEIC is refused by decision**: libheif and libde265 are LGPL-3.0 ([](/design/licensing/)). WebP, other RAW families, and AVIF without the feature are recognised and refused.

Rawshift is pinned to a commit of its git `master`, not a crates.io release, because its gamut codecs are unreleased. Keep bumping that pin, and return to a crates.io release once Rawshift is stable.

Nothing is silent. An absent codec is `media::MediaError::UnsupportedFormat`; an unencodable `(tier, format)` pair is recorded as deferred and counted in the import summary while the asset's other derivatives still ship.
:::

### Video Previews

Video derivatives (first-frame still, H.264 preview) are the planned `capsule-core::media::video` half of the media module, separate because transcode needs its own toolchain. They are signed through the same `DerivativeManifest` path ([](#derivative-provenance)) as stills.

- **Toolchain.** Video derivatives come from `rawshift-video`, never from a codec Capsule links itself. Encode goes through hardware codecs behind Rawshift's `VideoEncoder` seam. FFmpeg is excluded from every build ([](/design/licensing/#the-video-transcoder-seam)).
- **First frame.** The extracted frame becomes a `DecodedImage`, so the thumbnail tier, the pixel budget and the LQIP are the still path's, unchanged.
- **Until `rawshift-video` ships.** A video sniffs to no still format and its import reports `DeferredNoCodec`, never a missing asset. The original is stored, signed and verified like any other file.

### Seekable Video Renditions

An original video is one sealed blob, so playing from the middle would mean fetching and decrypting everything before it. A **rendition** makes playback and seeking fetch only what is shown, without the server ever seeing a frame.

- **Shape.** The client transcodes the video to fragmented MP4 at the preview resolution and cuts it at keyframes into segments of about 4 seconds. Each segment is sealed as its own blob, and a **segment index** (each segment's start time, duration and content address) is sealed as one more. The index is the rendition's entry point and is named by a `preview`-role `DerivativeManifest` ([](#derivative-provenance)).
- **Playback.** The player fetches the index, then the segments around the playhead. A seek fetches the segment that holds the target time and its successor, never the original.
- **Produced on the client.** The server cannot transcode ciphertext, so the uploading device, or any later device with the album's write tier, produces the rendition through `rawshift-video` as above. Recent short clips get one automatically at import; others on first play or on request.
- **Derived data.** A rendition is a derivative: it is counted in quota like one ([](/design/quota/#accounting-model)), may be dropped and rebuilt, and never replaces the original.
- **What the server learns.** Segment count and sizes, which reveal a video's length and rough bitrate. A video's length is already implied by its original's size, so a rendition adds no new class of fact.

### Embedded Previews

Camera RAW files usually carry a camera-rendered JPEG, anywhere from thumbnail size to near full resolution depending on camera and format. Rawshift extracts it without developing the image (`RawFile::thumbnail()`) for every RAW format the build decodes, so a device holding the original can show a large view at once, before any derivative is encoded. The extracted JPEG is whichever one the format designates, not always the largest in the file, and its size decides which rung it can fill.

- **Local only.** An embedded preview is read from the original on the device that holds it and is never uploaded, signed or listed as a tier. It is not a `DerivativeManifest.format`, so the no-JPEG rule for delivered tiers is unaffected. Other devices get the encoded tiers ([](#thumbnail-and-preview-formats)).
- **A stand-in, not a replacement.** On the render ladder ([](/design/transfer/download/#render-ladder)) it fills each rung it is large enough for until that rung's encoded tier exists. It fills Thumbnail when its long edge reaches the Thumbnail tier's. It fills Preview when it covers the surface showing it in device pixels, because the Preview tier is kept at source resolution and an embedded JPEG rarely matches that. Import still generates the derivatives. It is the camera's rendering, not Capsule's, so an encoded Preview replaces it once available.
- **Bounded like any decode.** It goes through the same pixel budget and sandbox as every other input. An embedded preview that is absent or does not decode leaves the rung to the next source.

## LQIP

Capsule imports Chromahash **0.7.1** directly, never through Rawshift; the pin lives in the root `Cargo.toml`. ThumbHash is retired (ADR-0003).

The chromahash, its format version and a `dominant_color` fallback form the `lqip` sidecar field ([](/design/metadata/#sidecar-schema-v1)) inside the encrypted metadata blob ([](/design/cryptography/encryption/#metadata-encryption)). The placeholder therefore arrives with metadata sync, before any thumbnail, and never reaches the server. A decoder that does not recognise the format version, or bytes `from_bytes` rejects, MUST fall back to the solid `dominant_color` fill. Changing the encoding requires a new format version: `lqip` is signed, and byte length alone cannot discriminate encodings.

### Encoding Tier

Always Chromahash's `DEFAULT_TIER`, **exactly 32 bytes**, never varied per asset, so the signed sidecar's size is predictable. Rendering uses `decode_capped` at the painted box size; the fill uses `average_color()`.

### Where LQIP Lives

`capsule-core::lqip`, not `capsule-core::media`: `media` is native-only, while the LQIP must reach the import pipeline, the uniffi FFI and the browser via `capsule-wasm` from one implementation.

## Derivative Provenance

Derivatives are regenerable but not unowned: adding or replacing one is a signed lifecycle action, so a buggy or hostile client cannot silently swap a thumbnail. [](/design/cryptography/provenance/#derivative-provenance) and [](/design/authorization/#the-closed-action-set) own the manifest structure and the `derivative-add` / `derivative-replace` actions. This page owns only the `format` values, checked under the closed-enum rule ([](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).

- A receiver MUST NOT schedule regeneration for a tier the sidecar's `dimensions` show the original already satisfies; the `original` sentinel is local and never shipped.
- A derivative whose manifest fails verification is discarded (not quarantined) and regenerated from the original, appending a new `derivative-replace` record.

## Known Gaps

- Seekable video renditions are not implemented; they wait on `capsule-core::media::video`.

## Validation

- **Format detection / closed enum (unit).** Each table row is identified correctly; malformed AVIF and `format = "image/future-codec"` are rejected.
- **Delivery fallback (unit).** No JXL decoder → AVIF; no AVIF → WebP.
- **LQIP (unit).** Payload is 32 bytes; `decode_capped` honours the bound; unknown version or invalid bytes → `dominant_color`.
- **Manifest verification (smoke).** Corrupted derivative is discarded, regenerated, and a `derivative-replace` record appended.
- **Original fallback (unit).** A small original yields `format = "original"`, not a redundant derivative.

The end-to-end case is in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
