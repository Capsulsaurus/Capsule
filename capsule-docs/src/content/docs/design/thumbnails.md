---
title: Thumbnails and Previews
description: How we generate and manage thumbnails and previews for media assets in Capsule
---

We generate thumbnails and previews for all photos and videos. This doc is the **single source of truth** for the LQIP scheme and the thumbnail/preview formats — per the [single-source-of-truth rule](/design/principles/#single-source-of-truth), other docs reference these by link rather than restating the choice.

## Thumbnail and Preview Formats

> **Status:** The format table below is **provisional**. The choice between AVIF and JXL as the primary still-image codec is pending field testing of decoder availability and quality-per-byte across Capsule's target devices in 2026. The single-source-of-truth structure means any later swap is a one-row edit here, propagated nowhere else — see [Single Source of Truth](/design/principles/#single-source-of-truth).
<!-- TODO: ^^ -->

Three derivative tiers per photo asset and one preview tier for video assets:

| Tier                                       | Photo format                                                | Video format                                                        | Notes                                                                                                                                                                                           |
| ------------------------------------------ | ----------------------------------------------------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Thumbnail** (grid display)               | **AVIF** (primary), WebP fallback for browsers without AVIF | First-frame AVIF still                                              | AVIF q=50, 4:2:0 chroma, ~256 px long edge.                                                                                                                                                     |
| **Preview** (lightbox / single-asset view) | **AVIF** (primary), WebP fallback                           | **H.264 baseline** transcode at original resolution capped to 1080p | AVIF q=70 for stills; H.264 CRF 23 for video, 30 fps cap, AAC audio.                                                                                                                            |
| **Desktop-only optional cache**            | **JXL**                                                     | (n/a)                                                               | JXL is generated only when the client is a desktop and the user opts in — best quality-per-byte but decoder support is still uneven in 2026. Never produced for shared/server-side derivatives. |

- **AVIF** is the primary because in 2026 it ships in every major browser and on every major OS (iOS 16+, Android 12+, Chrome/Firefox/Safari current). Hardware decode is widespread.
- **WebP** is the fallback for the rare client that lacks AVIF. We deliberately do not fall back to JPEG — WebP covers everything JPEG would.
- **JXL** is kept as a *desktop-only optional* tier rather than the primary because cross-platform decoder coverage is still patchy. It is purely a local-cache choice; remote/sharing paths never use JXL.
- **H.264 baseline** for video previews — universally decodable, cheap CPU/GPU cost on every platform. AV1 was considered but encode cost is still high on mobile in 2026.

If an original asset is lower-resolution than the highest thumbnail tier, the affected tier simply references the original instead of generating a redundant derivative. This is **distinct** from a missing derivative (an unintentional failure during generation) — the recovery-first principle treats missing derivatives as rebuildable from the original.

## LQIP

We use [chromahash](https://github.com/justin13888/chromahash) as a perceptual hash that decodes into a low-quality image placeholder. Chromahash was chosen for its color accuracy across color spaces and it was precisely developed for Capsule's particular needs. The hash is inlined into the encrypted CBOR metadata blob (see [Metadata Encryption](/design/cryptography/#metadata-encryption)), so it is available the instant metadata syncs, before any thumbnail fetch.

Considered and rejected: ThumbHash (smaller wire size but worse color fidelity for the wide-gamut and HDR sources Capsule expects), BlurHash (older, blurrier, less color-accurate). The single-LQIP choice avoids exactly the kind of "chromahash/ThumbHash" hedge that previously caused doc drift.

## Derivative Provenance

Thumbnails and previews are *ephemeral by recovery posture* (they can always be regenerated from the original) but not *unowned*. A buggy or hostile client could otherwise quietly replace a good thumbnail with a corrupted one, and the receiving side would have no way to tell. To prevent this, every thumbnail and preview is uploaded as a derivative whose addition or replacement is an authorized, signed lifecycle action.

The full derivative manifest structure and the `derivative-add` / `derivative-replace` action set are owned by [Cryptography — Derivative Provenance](/design/cryptography/#derivative-provenance) and [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set); this doc owns only the *format* of the derivative bytes. The two interact at exactly one point: the `DerivativeManifest.format` field names the codec/format from the table above, and the verifying side rejects a manifest whose `format` is not currently recognized (the closed-enum rule from [Threat Model — Schema Evolution](/design/threat-model/#schema-evolution-and-field-grammar)).

A thumbnail whose `DerivativeManifest` fails verification is **regenerated locally from the original** rather than trusted — the [recovery-first principle](/design/principles/) means a derivative is always rebuildable, so refusal-and-regenerate is the safe default. The corrupt copy is discarded (not quarantined — it carries no irreplaceable bytes), and the corresponding regeneration appends a new `derivative-replace` provenance record.
