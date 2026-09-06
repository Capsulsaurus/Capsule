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
- `src/data/server/` — `createBrowserServerGateway`, the real key-free gateway
  (slice `S-D6`). It drains the sync feed into an IndexedDB-backed client store
  and answers reads from it — the browser's `library.sqlite` analogue.
- `src/data/hooks.ts` — TanStack Query hooks (`useAssets`, `useAlbum`, …). UI
  components consume **only** these, never a data source directly.
- `src/data/index.ts` — selects the active gateway. **The mock gateway is
  retired**; there is no in-memory fallback.

Reads return key-free shells: ids, album membership and counts, and
awaiting-original state are real; titles, cover art, capture dates, dimensions,
LQIP and locations live in encrypted metadata and stay absent until a decode and
verify boundary lands *below* `CapsuleGateway`. That boundary is post-v1. With no
reachable server the store stays empty and the app renders empty states rather
than failing.

**Transitional.** The gateway currently drains the `capsule.sync.v1` gRPC-web
feed served by the Salvo server. That transport is retired: it is replaced by the
Kynos REST sync surface consumed through the Spargen-generated SDK, and the
gateway's transport layer (`src/data/server/sync/`) is rewritten against it in
the same change that moves the server. Until then this path works and is tested;
it is not dead code.

## Development

### Prerequisites

- Install [Bun](https://bun.sh).
- A running server for anything beyond empty states. `mise run serve-memory` from the repo root
  starts one on the in-memory adapters — an account registers and signs in against it, and it
  loses everything but the blobs when it exits. There is no mock gateway to fall back on; the
  sync store's own tests (`bun test src/data/server/`) exercise the data path without a server.

With no reachable server the app still builds, runs, and renders empty states, so
pure UI work needs no backend. Authenticated writes are not a web surface: the
browser client is read-only plus guest drops.

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

Today the client reads the `capsule.sync.v1` gRPC-web feed directly, through the
hand-written transport in `src/data/server/sync/`. That is transitional: the
target is the checked-in Kynos REST/OpenAPI contract consumed through the
Spargen-generated SDK, which replaces `sync/transport.ts` without changing
`CapsuleGateway` or anything above it. The Kynos server and its regenerated SDK
do not exist yet, so the current path stands until they do.

Guest drops and the share-link viewer do not go through this gateway at all —
they seal and open in the browser via `capsule-wasm`, and are unaffected by the
transport change.
