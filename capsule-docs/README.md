# Capsule Docs

Docs for Capsule. Built with Astro and Starlight.

## Hosting

Hosted on Cloudflare Pages through its Git integration: Cloudflare builds every push, so no
deploy credential lives in this repository. `master` is production, at `/`. Each release's
docs are a `docs/vX.Y` branch (cut by `release.yml`), deployed on its own and served by
production under `/v/X.Y/`. The workspace rustdoc is published to GitHub Pages by
`.github/workflows/rustdoc-pages.yml` and served under `/rustdoc/`. Both are proxied by the
Pages Functions in `functions/`; the URL layout is `src/lib/docs-version.mjs`.

### Cloudflare Pages settings

| Setting | Value |
| --- | --- |
| Project | `capsule`, at `capsule-22k.pages.dev` (`PAGES_PROJECT` in `src/lib/docs-version.mjs`) |
| Production branch | `master` |
| Preview branches | `docs/v*`, plus any branch whose preview you want |
| Root directory | `capsule-docs` |
| Build command | `bun install --frozen-lockfile && bun run build` |
| Build output directory | `dist` |

## Dependencies

`package.json` cannot hold comments, so the rationale for its pins lives here. Nothing here ships
in a client or server artifact.

- **Astro + `@astrojs/starlight`:** one chrome for every page, generated reference included.
- **`starlight-blog`:** posts are docs pages under `blog/`; MIT.
- **Web fonts:** no font package. Upstream Geist 1.9 variable files are vendored in
  `src/assets/fonts/`, so text never waits on a third-party origin; OFL-1.1 (see the
  licensing design doc).
- **`rehype`:** rebases a release build's links under `/v/X.Y/` after the build
  (`src/lib/base-links.mjs`).
- **`beautiful-mermaid`:** compiles Mermaid fences to themed SVG at build time, so no diagram
  runtime ships to the browser.
- **`github-slugger`, `mdast-util-from-markdown`, `mdast-util-to-string`, `unist-util-visit`:**
  what Astro's own Markdown pipeline uses, named directly because the remark plugins in `src/lib`
  import them: the same slugger Astro gives headings, so an empty-label link resolves exactly the
  anchor the page renders.

## Development

Start development server:

```bash
bun install
bun dev
```

Build the site and serve it with the Pages Functions (`/rustdoc/`, `/v/`), as Cloudflare does:

```bash
bun run preview
```

A push deploys: there is no deploy command. To build a release's docs locally, set the branch
Cloudflare would: `CF_PAGES_BRANCH=docs/v0.1 bun run build`.
