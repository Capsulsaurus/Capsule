---
paths:
  - "capsule-wasm/**"
---

# capsule-wasm

## Rules

- `capsule-wasm/wasm-surface.d.ts` is the committed browser surface: regenerate with `mise run wasm-surface`, checked by `wasm-surface-check` (part of `check-web`).
- It serves the read-only web client only ([clients.md](clients.md)): opening share links and sealing guest drops.

## Docs

Follow:

- [Share links](../../capsule-docs/src/content/docs/design/share-links.md) — opening a share in the browser.
- [Web upload](../../capsule-docs/src/content/docs/design/web-upload.md) — guest drop sealing; the KAT fixtures come from `mise run drop-kat` / `share-kat`.

Keep current:

- `capsule-wasm/wasm-surface.d.ts` (`mise run wasm-surface`) and the WASM section of the [SDK reference](../../capsule-docs/src/content/docs/reference/sdk.md).
