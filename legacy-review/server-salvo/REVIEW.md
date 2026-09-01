# Salvo Server Review Notes

## Potentially reusable behavior

- Authentication primitives and service-level account flows.
- Upload offset, checksum, terminal-state, and cleanup ideas.
- Filesystem staging, reflink fallback, and storage test fixtures.
- Postgres entities, queries, migrations, and Valkey test doubles as migration input only.

## Must be redesigned

- Kynos owns HTTP routing, OpenAPI, runtime startup, configuration integration, middleware,
  observability, and graceful shutdown.
- Public server APIs are REST/OpenAPI only.
- The server accepts opaque ciphertext blobs plus the key-free manifest envelope. It must never
  receive or infer a filename, capture time, dimensions, EXIF, tags, LQIP, or other plaintext media
  metadata.
- `AuthStateStore` and `UploadSessionStore` remain separate Capsule-owned contracts with Postgres,
  Valkey, and deterministic in-memory adapters.
- Blob layout, verification, quarantine, reference safety, and garbage collection remain a
  Capsule-owned implementation behind an arbitrary-backend trait.

## Do not reuse

- Salvo handlers, response writers, OpenAPI registration, or configuration projections.
- Server-side media decoding or metadata extraction. Those files were deleted during quarantine.
- The plaintext asset schema, transformation endpoints, filename-based storage layout, or upload
  finalization that marks an asset visible before the complete encrypted bundle is durable.

The disabled manifests are historical context only and are intentionally not valid workspace
packages.
