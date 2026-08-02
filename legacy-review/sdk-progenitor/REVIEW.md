# Progenitor SDK Review Notes

The generated Progenitor client and its handwritten authentication/upload wrapper are inactive.
They may inform workflow tests, but they are not an API contract.

The replacement must:

- Consume a checked-in OpenAPI 3.1 document emitted deterministically by Kynos.
- Pass `spargen check` and generated-surface compatibility checks in CI.
- Stream ciphertext uploads and range downloads without buffering complete blobs.
- Preserve typed documented errors and raw context for unknown server errors.
- Keep token refresh, upload, sync, recovery, and protocol-version orchestration outside generated
  code.

Do not revive `generate_openapi.sh`, Progenitor macros, or the assumed `openapi.json` path.
