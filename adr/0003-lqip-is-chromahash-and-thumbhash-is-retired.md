# ADR-0003 — LQIP is Chromahash, imported directly, and ThumbHash is retired

- **Status:** accepted
- **Date:** 2026-08-31
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Thumbnails — LQIP](../capsule-docs/src/content/docs/design/thumbnails.md#lqip)
- **Slices:** S-B14

## Context

The low-quality image placeholder had two implementations and no single owner. ThumbHash
stood in while Chromahash was unreleased: the `thumbhash` Rust crate sat behind
`capsule-core`'s `media` feature, and the npm `thumbhash` package decoded in
`capsule-web`'s `lazy-image.tsx`. `AGENTS.md` gated Chromahash to "after its v1 release",
and `xtask`'s architecture check forbade the crate outright.

Carrying two encodings for one signed field caused exactly the drift a hedge causes: the
docs said "chromahash/ThumbHash" in places, and the schema said something else again.

## Decision

Capsule imports **Chromahash 0.7.1** directly, never through Rawshift, and ThumbHash is
retired rather than excepted. Encode and decode live in `capsule-core::lqip`, a dedicated
module deliberately outside the retiring `capsule-core::media` stack, so one implementation
serves all three places a placeholder is produced or consumed: the import pipeline, the
native apps through the uniffi FFI, and the browser through `capsule-wasm`.

The `AGENTS.md` gate that read "after its v1 release" is **amended to 0.7.1** — the release
the project accepts as ready. A check that forbids an approved dependency has stopped
describing a decision and started blocking one, so `xtask`'s architecture check stopped
listing `chromahash` at the same time.

## Consequences

- The sidecar `lqip` format version stays `1` and `sidecar_schema` does not move. That is
  legitimate only because the migration is **total**: no persisted sidecar carries a
  ThumbHash payload, no fixture or known-answer vector pins one, and the schema always
  named the field `Lqip.chromahash` with format version 1 declared as the *chromahash*
  version. ThumbHash bytes were standing in for an unreleased dependency; they were never
  the declared encoding.
- The migration must stay total. ThumbHash payloads are shorter than 32 bytes and overlap
  the lower Chromahash tiers in length, so byte length alone cannot discriminate a stale
  one. If such a sidecar ever existed, the fix is a *new* format version, never a
  redefinition of this one.
- `thumbhash` stays in `xtask`'s retired-dependency list so it cannot return, on either
  side: the Rust crate and the npm package both go.
- The browser has no JavaScript placeholder codec and will not grow one. When it has a
  decrypted `lqip` to render, it decodes through `capsule-wasm` over the same
  `capsule-core::lqip` code.

## Considered and rejected

- **ThumbHash on its merits** — smaller wire size, worse colour fidelity for the wide-gamut
  and HDR sources Capsule expects.
- **BlurHash** — older, blurrier, less colour-accurate. Never adopted.
- **Keeping both behind a feature flag** — the hedge that caused the doc drift above.
