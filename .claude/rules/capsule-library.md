---
paths:
  - "capsule-library/**"
---

# capsule-library

## Rules

- `capsule-library` owns local state — SQLite, files, the `Workspace`, hardware signers — so that `capsule-core` stays stateless ([Module Map — Stateless Core](../../capsule-docs/src/content/docs/design/module-map.md#stateless-core)).
- Media decoding, format sniffing, EXIF and derivatives come from Rawshift through `capsule-core::media` — including in `exif`, `file_metadata.rs` and `lifecycle/derivatives`. Do not reimplement them here; the rule is in [capsule-core.md](capsule-core.md).

## Docs

Follow, by module (`capsule-library/src/…`):

| Module | Doc |
| --- | --- |
| `library/` (cache, space, paths), `db/` | [Filesystem — Client](../../capsule-docs/src/content/docs/design/filesystem/client.md) |
| `library/scrub`, `db/migrate.rs` | [Filesystem — Maintenance](../../capsule-docs/src/content/docs/design/filesystem/maintenance.md); `db/migrate.rs` also [Versioning](../../capsule-docs/src/content/docs/design/versioning.md) |
| `library/receipts`, `library/attestations`, `library/storage_verify` | [Durability](../../capsule-docs/src/content/docs/design/transfer/durability.md) |
| `library/auth_gate` | [Local gallery](../../capsule-docs/src/content/docs/design/local-gallery.md) |
| `import/` | [Import pipeline](../../capsule-docs/src/content/docs/design/import/pipeline.md); `import/camera/` also [Camera import](../../capsule-docs/src/content/docs/design/import/camera-import.md) |
| `account` | [Keys](../../capsule-docs/src/content/docs/design/cryptography/keys.md) |
| `lifecycle/` | [Authorization](../../capsule-docs/src/content/docs/design/authorization.md) (closed action set), plus per action: `import` → [Import pipeline](../../capsule-docs/src/content/docs/design/import/pipeline.md); `derivatives` → [Thumbnails](../../capsule-docs/src/content/docs/design/thumbnails.md); `metadata` → [Metadata](../../capsule-docs/src/content/docs/design/metadata.md); `organize`, `album` → [Organization](../../capsule-docs/src/content/docs/design/organization.md); `sync_apply` → [Sync](../../capsule-docs/src/content/docs/design/sync.md); `backup` → [Backup and recovery](../../capsule-docs/src/content/docs/design/backup-recovery.md); `enrollment` → [Device enrollment](../../capsule-docs/src/content/docs/design/device-enrollment.md); `drops` → [Web upload](../../capsule-docs/src/content/docs/design/web-upload.md); `provenance` → [Provenance](../../capsule-docs/src/content/docs/design/cryptography/provenance.md); `upload` → [Upload](../../capsule-docs/src/content/docs/design/transfer/upload.md); `sharing` → [Share links](../../capsule-docs/src/content/docs/design/share-links.md) |
| `db/` (embeddings, model inventory) | [AI](../../capsule-docs/src/content/docs/design/ai.md) |

Keep current:

- [Google Photos migration guide](../../capsule-docs/src/content/docs/guides/google-photos-migration.md) — any change to `import/importers/` that a user would see.
