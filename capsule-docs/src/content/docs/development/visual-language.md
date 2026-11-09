---
title: Visual Language
description: The brand tokens every Capsule surface shares, how each platform adopts them, and the rules the docs site follows
status: draft
---

This doc owns Capsule's **brand tokens**: the accent, neutral ramp, type, radius and spacing scales
defined in one place. It also owns the **voice** of public copy and the **docs-site
rules** (header, status badges, sidebar, front page, footer, blog). It defers everything else about a client's look to
[Clients — Design Language](/design/clients/#design-language): native material on each platform,
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
`capsule-swift/Modules/CapsuleUI/Sources/Glass/CapsuleTheme.swift`.

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
  Blog. The current section is an accent-tinted pill. Then search, a labelled GitHub button and a
  theme toggle that cycles Auto, Light and Dark. Narrow screens move the section links into the
  menu; splash pages such as the front page have no menu, so there they are reached through the
  page's own links.
- **Status badges.** Every authored page carries a review `status`
  ([Core Principles — Review Status](/design/principles/#review-status)). The page title always shows
  it. The sidebar shows it only for a page that is not `stable`, so the navigation lists exactly what
  still awaits review.
- **Sidebar.** The same surface as the page, divided from it by a hairline; the current page is an
  accent-tinted pill.
- **Front page.** In order: a hero with the differentiators and a theme-matched product screenshot,
  a one-line status, the differentiators with a link named for the doc behind each, the
  table-stakes tier, why Capsule is built this way, and where to start.
- **Footer.** On every page, centred: links to the documentation, the blog (once it is live), the
  roadmap, discussions and the source, then the copyright holder, the licence line and the commit
  the site was built from, linked to that commit.
- **The blog goes live with its first post.** Until a post is published the production site has no
  blog route, header link, RSS feed, sitemap entry or footer link, so no reader lands on an empty
  index.
- **Blog.** Announcements, release notes and design write-ups. A post links to the design doc it
  discusses rather than restating its contract. `draft: true` keeps a post unpublished; a published
  post carries `status: stable`, so it never shows a Draft badge. A published post also carries a
  title, a description of 50 to 160 characters, a date, at least one configured author and one tag,
  and an `<!-- excerpt -->` marker that ends its teaser on the index. A post's header shows its
  date, reading time and authors with their pictures, and its body is set to a reading measure.

## Validation

- `scripts/page-status.test.mjs` fails on any docs page without a `status`, and on a published blog
  post whose status is not `stable` or that breaks the post contract above
  (`src/lib/blog-posts.test.mjs` proves the contract check itself).
- `src/lib/site-nav.test.mjs` proves the header and footer links, their order, the Blog link's
  dependence on the blog being live, and that every internal link resolves to a page.
- `src/lib/theme-cycle.test.mjs` proves the toggle's cycle and that it stores what Starlight reads;
  `src/lib/build-commit.test.mjs` proves which commit the footer names.
- `src/lib/sidebar-status.test.mjs` proves which sidebar entries carry a badge.
- `mise run build-docs` runs the link validator over every page, front-page feature links included.
  The header and footer components are not seen by the link validator; their links are checked by
  `site-nav.test.mjs` instead.
- Contrast figures above are recomputed when the accent changes; there is no automated contrast gate.
