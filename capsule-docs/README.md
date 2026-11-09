# Capsule Docs

Docs for Capsule. Built with Astro and Starlight.

## Hosting

Cloudflare Pages builds every push; how `master`, the `docs/vX.Y` release branches and rustdoc
are served is [Hosting and Versions](src/content/docs/design/developer-docs.md#hosting-and-versions).

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
