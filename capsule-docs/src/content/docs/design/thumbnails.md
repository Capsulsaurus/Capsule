---
title: Thumbnails and Previews
description: Format inventory, LQIP scheme, and derivative provenance for photo and video derivatives
---

We generate thumbnails and previews for all photos and videos. This doc is the **single source of truth** for the LQIP scheme and the thumbnail/preview formats — per the [SSoT rule](/design/principles/#single-source-of-truth), other docs reference these by link rather than restating the choice. The format table is itself the contract: every receiver (and every federated peer) compares the `DerivativeManifest.format` value against this list, and an unknown value is a structural rejection.

Derivative generation runs client-side in `capsule-sdk` (per-platform encoder libraries) over the shared format-detection and manifest-building logic in `capsule-core`. Server-side serving is `capsule-api-media` (it serves opaque ciphertext — never decodes).

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

If an original asset is lower-resolution than the highest thumbnail tier, that tier references the original instead of generating a redundant derivative. This is **distinct** from a missing derivative (an unintentional generation failure): the tier's [`DerivativeManifest`](/design/cryptography/provenance/#derivative-provenance) carries the recognized sentinel `format = "original"` — an explicit, signed marker the receiver trusts — whereas a simply-absent derivative is treated as rebuildable from the original (recovery-first).

## LQIP

We use [chromahash](https://github.com/justin13888/chromahash) as a perceptual hash that decodes into a low-quality image placeholder, chosen for its color accuracy across color spaces and developed for Capsule's needs. The chromahash, its format version, and a `dominant_color` fallback are the [`lqip` field of the sidecar](/design/metadata/#sidecar-schema-v1) — inside the [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption), so the placeholder is available the instant metadata syncs, before any thumbnail fetch, and never leaks to the server. A decoder that does not recognize the chromahash format version falls back to the solid `dominant_color` fill rather than misrendering, so a future chromahash revision is a versioned change, never a silent break.

Considered and rejected: ThumbHash (smaller wire size but worse color fidelity for the wide-gamut and HDR sources Capsule expects), BlurHash (older, blurrier, less color-accurate). The single-LQIP choice avoids exactly the kind of "chromahash/ThumbHash" hedge that previously caused doc drift.

## Derivative Provenance

Thumbnails and previews are *ephemeral by recovery posture* (they can always be regenerated from the original) but not *unowned*. A buggy or hostile client could otherwise quietly replace a good thumbnail with a corrupted one, and the receiving side would have no way to tell. To prevent this, every thumbnail and preview is uploaded as a derivative whose addition or replacement is an authorized, signed lifecycle action.

The full derivative manifest structure and the `derivative-add` / `derivative-replace` action set are owned by [Cryptography — Derivative Provenance](/design/cryptography/provenance/#derivative-provenance) and [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set); this doc owns only the *format* of the derivative bytes. The two interact at exactly one point: the `DerivativeManifest.format` field names the codec/format from the table above, and the verifying side rejects a manifest whose `format` is not currently recognized (the closed-enum rule from [Threat Model — Schema Rules](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).

A thumbnail whose `DerivativeManifest` fails verification is **regenerated locally from the original** rather than trusted — the [recovery-first principle](/design/principles/) means a derivative is always rebuildable, so refusal-and-regenerate is the safe default. The corrupt copy is discarded (not quarantined — it carries no irreplaceable bytes), and the corresponding regeneration appends a new `derivative-replace` provenance record.

## Validation

- **Format detection (unit).** Encode a derivative under each row of the format table; assert the format is correctly identified by the consumer (browser tier, native client tier). Negative: provide a malformed AVIF; assert structural rejection.
- **Closed-format enum (unit).** Submit a `DerivativeManifest` with `format = "image/future-codec"`; assert rejection at the envelope check.
- **JXL-to-AVIF delivery fallback (unit).** Simulate a consumer without a JXL decoder; assert it selects the AVIF variant (and a consumer without AVIF selects WebP), never failing to render a tier that exists.
- **LQIP round-trip (unit).** Generate chromahash for a fixture image; assert decoded LQIP matches expected pixel buffer within quality tolerance, and that an unrecognized chromahash format version falls back to `dominant_color`.
- **Derivative-manifest verification (smoke).** Upload a derivative; corrupt the bytes; refetch; assert the receiver discards and regenerates from the original; assert a new `derivative-replace` provenance record is appended.
- **Original-fallback (unit).** Provide an original smaller than the highest thumbnail tier; assert that tier's manifest carries `format = "original"` rather than generating a redundant derivative.

The cross-module case — derivative generation → upload → fetch → display — is covered by the upload+sync E2E case in [Module Map](/design/module-map/#e2e-test-surface).
