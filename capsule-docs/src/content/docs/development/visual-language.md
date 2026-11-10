---
title: Visual Language
description: The brand tokens every Capsule surface shares, how each platform adopts them, and the rules the docs site follows
status: draft
---

This doc owns Capsule's **brand tokens**: the accent, neutral ramp, type, radius and spacing scales
defined in one place. It also owns the **voice** of public copy and the **docs-site
rules** (header, status badges, sidebar, front page, footer, blog). It defers everything else about a client's look to
[](/design/clients/#design-language): native material on each platform,
no third house style. The brand reaches a native client only through its accent and its app icon.

## Principles

- **Photos are the colour.** The interface stays quiet so that photographs carry the screen. The
  accent marks what to act on and small identity cues (an icon well, a selected tab), never a large
  surface.
- **Native material, shared identity.** Apple clients follow HIG and Android clients follow Material.
  The accent is the only brand token a native client adopts; type, controls, radius and spacing are
  the platform's own. The radius and spacing scales below are the Apple client's; the docs site
  mirrors the radii, and Android keeps Material's shape and spacing scales.
- **Nothing leaks.** A product surface that shows a user's library (the web client, share and
  guest-upload pages) MUST NOT request a third-party asset: fonts, images, scripts and analytics come
  from the same origin or are not used. The docs site is not a product surface: it self-hosts its
  fonts and scripts, and may show third-party images such as a blog author's GitHub avatar
  (`avatars.githubusercontent.com`).
- **Accessible by default.** Text and interactive accents meet WCAG AA (4.5:1) in both themes, motion
  is decorative only and stops under `prefers-reduced-motion`, and a state is never conveyed by
  colour alone.
- **Never a status colour.** The accent must not read as error, warning or success, so those keep
  their meaning wherever they appear.

## Tokens

### Accent

One accent, defined once as an OKLCH hue and chroma, from which every theme's three steps are derived
at fixed lightness. The current accent is **Slate Blue** (hue 255, chroma 0.09).

| Step | Light | Dark | Use |
| --- | --- | --- | --- |
| `low` | `#dce9fa` | `#19273a` | Tinted surfaces: callouts, icon wells, selected rows. |
| `accent` | `#3e6596` | `#7ea7dc` | Fills: primary buttons, the selected tab, focus rings. |
| `high` | `#173357` | `#a1c7f8` | Text in the accent colour: links, and text on a `low` surface. |

A light link is `accent` on white (6.0:1); a dark link is `high` on the dark background (10.4:1).
Changing the accent changes these six values together and nothing else.

### Neutrals

An 11-step ramp from `#f7fafe` to `#0c0d10`, tinted toward the accent hue at chroma 0.006 so greys sit
with the accent instead of against it. Light surfaces use the top of the ramp and dark surfaces the
bottom; body text is the opposite end.

### Type

- **Web and docs:** Geist for text and Geist Mono for code, self-hosted. The docs site vendors
  upstream Geist 1.9's variable cuts, upright and italic (`src/assets/fonts/`); at a named weight
  their outlines match the static cuts.
- **Apple and Android:** the platform's system text styles (Dynamic Type, Material type scale). No
  brand font ships in a native app.

### Radius and spacing

Radii are 8, 12, 16 and 22 points (small, card, medium, large), continuous corners where the platform
offers them. Spacing is a 4-point scale (2, 4, 8, 12, 16, 24, 32). Both match
`capsule-swift/Modules/CapsuleUI/Sources/Glass/CapsuleTheme.swift`. The docs site adds a pill
radius (`--capsule-radius-pill`) for fully rounded ends: header links and controls, badges and
tags.

## Adoption per Surface

| Surface | Where the accent lands | Everything else | Status |
| --- | --- | --- | --- |
| Docs site | `--capsule-accent-*` in `capsule-docs/src/styles/global.css`, mapped onto Starlight's `--sl-color-accent*`. | Starlight, restyled per the rules below. | Implemented |
| Apple | An `AccentColor` asset, read once through `CapsuleTheme.accent`. | SwiftUI system styles, SF Symbols, Liquid Glass. | Planned: the apps use the system tint |
| Android | The Material 3 seed colour, with the scheme generated from it and dynamic colour as a user option. | Material 3 components and type scale. | Planned: the app uses the default scheme |
| Web client | The shadcn `--primary` token, with Geist self-hosted. | Neutral shadcn tokens. | Planned: neutral primary, fonts from Google |

The docs favicon is a provisional capsule mark in the accent colour; the app icon replaces it once
one is designed.

## Voice

- **Lead with what is hard.** Headlines name what Capsule achieves that others do not: organization
  of very large libraries on the user's devices, a server that holds no keys, sharing across servers.
  Claims any photo service would make go in an "everything you would expect" tier, not a headline.
- **Address everyone.** No copy targets the users of a named competitor; importing from other
  services belongs under getting started.
- **Open source, not "free".** Say Capsule is open source and openly developed. The licence is written
  `AGPL-3.0`.
- **State where things are.** Describe the current state in present tense without hedging, and never
  claim a feature ships before it does. Designed-but-deferred work is described as designed.

## Docs Site

- **Header.** The wordmark (the capsule mark beside "Capsule" in heavy, tightly tracked Geist), then
  the section links centred in the free space: Guides, Design, Reference and, once the blog is live,
  Blog. The current section is an accent-tinted pill. Then search, a version switcher (always
  shown, naming the version being read: Latest, or `vX.Y`, with each released version once one
  exists, in a menu drawn like the site's other controls rather than the browser's native
  dropdown, the version being read an accent-tinted pill), a labelled GitHub button and a theme toggle that cycles Auto, Light and Dark. The bar
  is one even pane of tinted glass from its top edge to its bottom edge, the page blurred beneath
  it, so text scrolling under it fades uniformly rather than showing through at the edges. The bar
  is laid out by the window's width alone, so it is identical on every page, the front page
  included, and a navigation in a browser with view transitions keeps it in place. The section
  links need a wide window (72rem, where the table of contents also appears); narrower, the sidebar
  carries the sections, and below 50rem the menu carries the section links and the switcher.
  Splash pages such as the front page have no menu, so there the sections are reached through the
  page's own links.
- **Page transitions.** Moving between pages animates the content, never the chrome: the header
  holds still, the sidebar crossfades so only its current-page pill moves, and the content and table
  of contents step out before the new page's arrive, the content rising a few pixels into place. A
  step back through history reverses the direction, the new content settling down from above. It is
  a browser view transition, so a browser without one, or a reader who asks for reduced motion,
  navigates with no animation.
- **Background.** Every page carries a fine dot grid in its top-right corner, fixed to the window and
  dissolving toward the middle: 1px neutral dots, faint in both themes, so it reads as texture and
  never competes with text. It shows behind the content, the table of contents and the header's
  glass; the sidebar covers it, and it is not printed.
- **Status badges.** Every authored page carries a review `status`
  ([](/design/principles/#review-status)). The page title always shows
  it. The sidebar shows it only for a page that is not `stable`, so the navigation lists exactly what
  still awaits review.
- **Sidebar.** The same surface as the page, divided from it by a hairline; the current page is an
  accent-tinted pill. Where the table of contents appears beside it, the two columns are the same
  width and the content is centred between them, so the text has equal space on each side at every
  window width.
- **Front page.** In order: a hero with the differentiators and a theme-matched product screenshot,
  a one-line status, the differentiators with a link named for the doc behind each, the
  table-stakes tier, why Capsule is built this way, and where to start.
- **Footer.** On every page, centred: links to the documentation, the blog (once it is live), the
  roadmap, discussions and the source, then the copyright holder, the licence line and the commit
  the site was built from, linked to that commit. On a page shorter than the window it rests on
  the window's bottom edge; on a longer one it follows the content.
- **Generated reference.** A generated page says so under its title: the surface (REST API, CLI,
  an SDK), a dashed "Generated" tag and its source, and for REST a download of the OpenAPI
  document. A REST page opens with an index of its endpoints, each a method pill and path, and its schemas, and lays
  every operation out as the reference (parameters and responses as field lists) beside a sticky
  panel with an example `curl` request and example success bodies. Method pills are coloured by
  method and status dots by class, from Starlight's semantic colours. Every other generated page
  is laid out the way its readers' own tool lays it out: DocC for Swift (kind eyebrow, abstract,
  Declaration, Overview, Topics), Dokka for Kotlin (name beside signature and KDoc), TypeDoc for
  the browser module (kind chips, Parameters, Returns, where it is defined) and a man page for the
  CLI (Synopsis, Description, Options). A generated page carries no review status.
- **The blog goes live with its first post.** Until a post is published the production site has no
  blog route, header link, RSS feed, sitemap entry or footer link, so no reader lands on an empty
  index.
- **Blog.** Announcements, release notes and design write-ups. A post links to the design doc it
  discusses rather than restating its contract. `draft: true` keeps a post unpublished; a published
  post carries `status: stable`, so it never shows a Draft badge. A published post also carries a
  title, a description of 50 to 160 characters, a date, at least one configured author and one tag,
  and an `<!-- excerpt -->` marker that ends its teaser on the index. A post's header shows its
  date, reading time and authors with their pictures, and its body is set to a reading measure.

## Docs Prose

- **Short and grouped.** One idea per sentence, related sentences in one paragraph, no filler; cut
  words, never technical depth.
- **Tables compare, lists enumerate.** A table when several items are compared on two or more
  attributes; a list when each item has one attribute or needs a sentence; prose for an argument.
- **The spec is the spec.** Request and response shapes, headers, statuses and per-endpoint rules
  live in the code that emits the OpenAPI document; a design doc states why and links the
  generated page.
- **Labels come from targets.** A link to a page or heading on this site is written with empty
  text, `[](/design/metadata/#identifiers)`, and renders the target's title, or for a heading
  "Title — Heading" with the em dash muted (a heading on the same page renders alone)
  (`src/lib/remark-link-titles.mjs`), so a label cannot drift from what it names. Write the
  sentence around the label, typically as a parenthetical after the term it supports. The build
  rejects such a link written with its own text; generated reference pages, and site paths that
  are not docs pages (`/rustdoc/`, `/openapi.json`), are exempt.
- **Libraries are badged once per page.** The first mention of a library or tool becomes a badge
  linking its source repository from `src/lib/library-links.mjs`: the name itself, a touch
  smaller, in a hairline pill with a code-brackets glyph, sitting on the line like any other word.
  It is never an underlined link, so an automatic substitution never looks like a link an author
  chose. A mention inside a larger word (`` `rustls`-only ``, a possessive, a path) is never
  badged, so a pill never splits a word; the next standalone mention is.
  Add a library there, not a URL to a page. A name that is also an ordinary word, or names two
  things (`gamut`), is linked by hand.
- **Diagrams are Mermaid.** A graph, flow or state machine is a ```` ```mermaid title="…" ````
  fence, compiled at build time to SVG in the page's colours (`src/lib/remark-mermaid.mjs`); the
  title is its caption and accessible name. Flowchart nodes take the house classes `client`,
  `server`, `store`, `external`, `danger` and `accent`. A literal directory layout stays a text
  tree.

## Validation

- `scripts/page-status.test.mjs` fails on any docs page without a `status`, and on a published blog
  post whose status is not `stable` or that breaks the post contract above
  (`src/lib/blog-posts.test.mjs` proves the contract check itself).
- `src/lib/site-nav.test.mjs` proves the header and footer links, their order, the Blog link's
  dependence on the blog being live, and that every internal link resolves to a page.
- `src/lib/version-menu.test.mjs` proves what the version switcher lists, where its keyboard focus
  moves and which side of the button it opens on.
- `src/lib/theme-cycle.test.mjs` proves the toggle's cycle and that it stores what Starlight reads;
  `src/lib/build-commit.test.mjs` proves which commit the footer names.
- `src/lib/sidebar-status.test.mjs` proves which sidebar entries carry a badge.
- `src/lib/remark-link-titles.test.mjs`, `remark-library-links.test.mjs` and
  `remark-mermaid.test.mjs` prove the link labels and their parts, the library badges and the
  diagram compiler; the build fails on an empty link whose target is missing, on a link to a docs
  page or heading written with its own text, and on a diagram that does not render.
- `scripts/gen-reference.test.mjs` proves the REST layout, the endpoint index's anchors, the
  example bodies and requests, and the generated-page banner; `src/lib/fonts.test.mjs` proves every
  `@font-face` names a vendored file.
- `mise run build-docs` runs the link validator over every page, front-page feature links included.
  The header and footer components are not seen by the link validator; their links are checked by
  `site-nav.test.mjs` instead.
- Contrast figures above are recomputed when the accent changes; there is no automated contrast gate.
