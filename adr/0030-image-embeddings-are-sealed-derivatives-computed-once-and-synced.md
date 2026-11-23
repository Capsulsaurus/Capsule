# ADR-0030 — Image embeddings are sealed derivatives, computed once and synced

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [AI § Synced Embeddings](../capsule-docs/src/content/docs/design/ai.md#synced-embeddings), [AI § Embedding Provenance](../capsule-docs/src/content/docs/design/ai.md#embedding-provenance)
- **Issues:** #1286

## Context

The AI doc made every device compute its own embeddings and required byte-identical inference
across devices so their vectors were comparable. A device that could not reach bit-exactness
got a `platform` partition and regenerated everything itself. In practice every phone, tablet
and laptop of an account indexed the whole library independently. On a library of a million
photos that is a battery and thermal cost paid once per device, and an album co-member indexed
the same photos again.

The derivative machinery already had an `embedding` role with `model_id`/`model_version`
(`DerivativeManifest`, Cryptography — Provenance), but nothing used it.

Prior art: an end-to-end encrypted photo service seals each file's ML output (face and CLIP
vectors) under that file's key and syncs it, so one device indexes and every other device and
sharee reuses the result. It recommends indexing on a desktop first for that reason.

## Decision

An image embedding is an `embedding`-role derivative: one canonical vector per
`(asset, model_id, model_version)`, published once by a device that passes the model's
known-answer check and holds the album's write tier, and fetched by every other device and
co-member before any inference runs. A second device's vector for a tuple that already has a
chain head is discarded, never chained. Bit-exactness is now a *publish* gate, not a
comparability requirement. Only the text-query encoder runs on every device.

## Consequences

- Indexing cost becomes once per asset instead of once per device; reader-tier co-members get
  semantic search over albums they cannot write.
- The `platform` partition stays only for vectors a device computed and could not publish.
- Embedding bytes are charged as derivatives. The size per asset is fixed by the model, so the
  server learns nothing it did not already know from the asset count.
- Face and text outputs need their own derivative roles, which is a closed-set change and
  therefore a `protocol_version` bump.

## Considered and rejected

- **Keep per-device inference.** Correct, but it multiplies the most expensive client job by
  the number of devices and leaves readers without an index.
- **Let the user choose per library.** Two code paths for no gain in privacy: the synced vector
  is sealed like any thumbnail.
