# Capsule Docs

Docs for Capsule. Built with Astro and Starlight.

<!-- TODO: Deploy to Cloudflare or GH Pages? -->

## Dependencies

`package.json` cannot hold comments, so the rationale for its pins lives here. Nothing here ships
in a client or server artifact.

- **Astro + `@astrojs/starlight`:** one chrome for every page, generated reference included.
- **`starlight-blog`:** posts are docs pages under `blog/`; MIT.
- **Web fonts:** no font package. Upstream Geist 1.9 variable files are vendored in
  `src/assets/fonts/`, so text never waits on a third-party origin; OFL-1.1 (see the
  licensing design doc).
- **`beautiful-mermaid`:** compiles Mermaid fences to themed SVG at build time, so no diagram
  runtime ships to the browser.

## Development

Start development server:

```bash
bun install
bun dev
```

Build production site:

```bash
bun run build
bun preview
```

Deploy to Cloudflare Pages:

```bash
bun run build
bun run deploy
```
