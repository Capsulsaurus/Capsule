# Capsule Web

A read-only-focused web client for Capsule, built with React 19, Rsbuild
(rspack), Tailwind CSS v4, TanStack Router/Query, shadcn/ui, and Biome.

Unlike the mobile/desktop apps, the web client has no platform-specific
behaviour and is primarily for **viewing** a library. Per the
[client design](../capsule-docs/src/content/docs/design/clients.md) it cannot
enroll devices, upload assets, or edit metadata — those require the
hardware-bound and write-tier keys a browser does not have.

## Architecture

The web app holds **no business logic of its own**. Validation, decryption,
sync, and the `verify_asset` chokepoint live in `capsule-core` and are surfaced
to clients through high-level APIs (`capsule-sdk` / the `capsule-api` server).
The UI reads through a thin, swappable boundary so those implementations can
drop in later:

- `src/domain/` — display types (`Asset`, `Album`). Deliberately thinner than
  capsule-core's model; each field notes its eventual source.
- `src/data/gateway.ts` — the read-only `CapsuleGateway` interface the UI
  depends on.
- `src/data/mock/` — `mockGateway`, the in-memory sample data backing the UI
  today.
- `src/data/server/` — `serverGateway`, a stub for the real adapter (pending
  the capsule-api E2E rework).
- `src/data/hooks.ts` — TanStack Query hooks (`useAssets`, `useAlbum`, …). UI
  components consume **only** these, never a data source directly.
- `src/data/index.ts` — selects the active gateway (mock for now).

When the server schema is live, implement `ServerGateway` against the checked-in
Kynos REST/OpenAPI contract through the Spargen-generated SDK and select it in
`data/index.ts`. If `capsule-core` later ships
a WebAssembly build, a decode/verify boundary slots in *below* `CapsuleGateway`
— assets would arrive as ciphertext references plus a `decode()` call — without
changing the UI.

## Development

### Prerequisites

- Install [Bun](https://bun.sh).
- The replacement API is not available yet; UI-only development uses local fixtures

The app runs against the mock gateway out of the box, so no backend is required
for UI work. Network-backed authentication and library reads remain planned.

### Commands

```bash
bun install        # install dependencies
bun dev            # start the dev server (http://localhost:5173)
bun run build      # production build
bun run preview    # preview the production build locally
```

Lint, format, test, and build together (matches CI):

```bash
mise run check-web
```

## Internationalization

User-facing strings come from the canonical `locales/` catalogs, compiled to
`src/i18n/messages/*.json` by `mise run i18n` (see the
[i18n design doc](../capsule-docs/src/content/docs/design/i18n.md)). Don't edit
the generated catalogs by hand.

## API

The web client will consume the checked-in Kynos REST/OpenAPI contract through the
planned Spargen-generated SDK. The replacement server and SDK are not available yet.
