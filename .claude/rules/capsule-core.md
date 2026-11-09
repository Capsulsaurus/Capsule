---
paths:
  - "capsule-core/**"
---

# capsule-core

## Rules

- `capsule-core` is stateless: pure functions over values, with time, randomness and storage injected through traits. Local state (SQLite, files, the `Workspace`, hardware signers) lives in `capsule-library`; network state in `capsule-sdk`. A new exception needs a bullet with a one-line reason in the [crate docs](../../capsule-core/src/lib.rs).
- Media decoding, metadata extraction and derivatives come from Rawshift (`rawshift-image` from crates.io, never the submodule) through `capsule-core::media`. Codecs in `default` are pure Rust; native code enters only through the opt-in `media-linked-codecs` and `media-avif-decode` features. Do not reimplement anything Rawshift does or aims to do (format sniffing, EXIF, resizing, video, hardware acceleration); fix it upstream. A format with no codec in the build is a typed `MediaError::UnsupportedFormat` for derivatives, never a silent gap, and never a reason to refuse storing the original. HEIC decode is refused (libheif and libde265 are LGPL-3.0). Contract: [Thumbnails](../../capsule-docs/src/content/docs/design/thumbnails.md).
- LQIP is Chromahash 0.7.1, imported directly and implemented once in `capsule-core::lqip`. ThumbHash is retired; neither the crate nor the npm package may return.

## Docs

Follow, by module (`capsule-core/src/…`):

| Module | Doc |
| --- | --- |
| everything | the [crate docs](../../capsule-core/src/lib.rs) (stateless core) |
| `crypto/` | [Cryptography](../../capsule-docs/src/content/docs/design/cryptography/index.md) and its sub-docs: [keys](../../capsule-docs/src/content/docs/design/cryptography/keys.md) (`keys/`, `authority/`, `verify_asset`), [encryption](../../capsule-docs/src/content/docs/design/cryptography/encryption.md) (`encryption/`), [MLS](../../capsule-docs/src/content/docs/design/cryptography/mls.md) and [MLS resilience](../../capsule-docs/src/content/docs/design/mls-resilience.md) (`authority/openmls_authority/`, `membership`), [provenance](../../capsule-docs/src/content/docs/design/cryptography/provenance.md) (`provenance/`); `upgrade.rs` → [Versioning](../../capsule-docs/src/content/docs/design/versioning.md); `receipts`, `storage_attestation` → [Durability](../../capsule-docs/src/content/docs/design/transfer/durability.md) |
| `sidecar/`, `metadata/`, `cbor/` | [Metadata](../../capsule-docs/src/content/docs/design/metadata.md), [Schema rules](../../capsule-docs/src/content/docs/design/threat-model/schema-rules.md); `sidecar/` also [Encryption](../../capsule-docs/src/content/docs/design/cryptography/encryption.md) and [Organization](../../capsule-docs/src/content/docs/design/organization.md) |
| `media/`, `lqip/`, `derivative_format` | [Thumbnails](../../capsule-docs/src/content/docs/design/thumbnails.md), [Licensing](../../capsule-docs/src/content/docs/design/licensing.md) |
| `validation/` | [Threat model — Validation](../../capsule-docs/src/content/docs/design/threat-model/validation.md) |
| `sharing/` | [Share links](../../capsule-docs/src/content/docs/design/share-links.md) |
| `drop/` | [Web upload](../../capsule-docs/src/content/docs/design/web-upload.md) |
| `federation/` | [Federation](../../capsule-docs/src/content/docs/design/federation.md) |
| `backup/` | [Backup and recovery](../../capsule-docs/src/content/docs/design/backup-recovery.md) |
| `ml/` | [AI](../../capsule-docs/src/content/docs/design/ai.md) |
| `notify/` | [Notifications](../../capsule-docs/src/content/docs/design/notifications.md); `notify/class.rs` also [Threat scenarios](../../capsule-docs/src/content/docs/design/threat-model/scenarios.md) |
| `cohort` | [Authentication](../../capsule-docs/src/content/docs/design/authentication.md) |
| `enrollment` | [Device enrollment](../../capsule-docs/src/content/docs/design/device-enrollment.md) |
| `import/` | [Import pipeline](../../capsule-docs/src/content/docs/design/import/pipeline.md), [Upload](../../capsule-docs/src/content/docs/design/transfer/upload.md) |
| `culling` | [Organization](../../capsule-docs/src/content/docs/design/organization.md) |
| `client_build` | [Provenance](../../capsule-docs/src/content/docs/design/cryptography/provenance.md) |

Keep current:

- [Cryptographic primitives](../../capsule-docs/src/content/docs/design/cryptography/primitives.md) — every change to `crypto/primitives.rs`, `hash`, `kdf`, `pwkdf` or `rng`.
- The [crate docs](../../capsule-core/src/lib.rs) — any new stateful exception.
- `capsule-docs/planned-modules.txt` — remove a row once its module exists.
