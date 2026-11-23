// Remark plugin: a site link written with empty text renders its target's own
// words, so a link label can never drift from the page or heading it names.
//
//   [](/design/metadata/)               -> "Metadata"            (page title)
//   [](/design/metadata/#identifiers)   -> "Metadata — Identifiers"
//   [](#identifiers)                    -> "Identifiers"         (same page)
//
// Titles come from the target's frontmatter, headings from its Markdown,
// slugged as Astro slugs them (github-slugger over the heading text). A link
// whose target or anchor does not exist fails the build: an empty label has
// no text to fall back to.
//
// The convention is enforced: a link to a docs page or heading written with
// its own text fails the build too, so no label can be hand-written and
// drift. Generated reference pages (frontmatter `generated:`) are exempt;
// they label symbol links with the symbol. Links off the site, and site paths
// that are not docs pages (/rustdoc/, /openapi.json), keep their text.
//
// On a translated page (src/content/docs/<locale>/, src/lib/locales.mjs) a
// link stays in the page's locale: `[](/guides/x/)` points at
// `/<locale>/guides/x/` and renders that page's title, translated where the
// locale has the page and English where Starlight serves the English page as
// the locale's fallback. Translations write the English anchors; one that
// names a heading of the English page maps to the heading in the same place
// of a translation whose headings are laid out alike. When they are not, the
// link goes to the English page, which has that heading, rather than failing.
// See `localizeUrl`.
//
// A label renders as `a.xref` holding `span.xref-page`, `span.xref-sep` and
// `span.xref-heading`, styled in global.css (Cross-references). The
// convention is a contract of the Visual Language doc
// (development/visual-language.md).
import { existsSync, readFileSync } from 'node:fs';
import { isAbsolute, join, relative, sep } from 'node:path';
import GithubSlugger from 'github-slugger';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString as textOf } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';
import { stripFrontmatter } from '../../scripts/lib/markdown.mjs';
import {
    contentFileForRoute,
    servingFileForRoute,
} from '../../scripts/lib/routes.mjs';
import { localizePath, splitLocale } from './locales.mjs';

/** Separates a page title from a heading on another page. */
export const SEPARATOR = ' — ';

/**
 * A page's title and its headings by anchor, in document order.
 *
 * @typedef {object} PageOutline
 * @property {string} title
 * @property {Map<string, string>} headings Anchor slug to heading text.
 * @property {number[]} [depths] Each heading's depth, in document order:
 *   the layout a translation's headings are matched against.
 */

/**
 * The `title` frontmatter field, unquoted.
 *
 * @param {string} source
 * @returns {string | null}
 */
export function frontmatterTitle(source) {
    const block = /^---\r?\n([\s\S]*?)\r?\n---/.exec(source);
    if (!block) return null;
    const line = /^title:\s*(.+?)\s*$/m.exec(block[1]);
    if (!line) return null;
    const raw = line[1];
    if (/^".*"$/.test(raw)) return JSON.parse(raw);
    if (/^'.*'$/.test(raw)) return raw.slice(1, -1).replace(/''/g, "'");
    return raw;
}

/**
 * Every heading in an mdast tree, keyed by the anchor Astro gives it.
 *
 * @param {import('mdast').Root} tree
 * @returns {Map<string, string>}
 */
export function headingsOf(tree) {
    const slugger = new GithubSlugger();
    const headings = new Map();
    visit(tree, 'heading', (node) => {
        const text = textOf(node).trim();
        headings.set(slugger.slug(text), text);
    });
    return headings;
}

/**
 * Each heading's depth (2 for `##`), in document order.
 *
 * @param {import('mdast').Root} tree
 * @returns {number[]}
 */
export function headingDepths(tree) {
    const depths = [];
    visit(tree, 'heading', (node) => {
        depths.push(node.depth);
    });
    return depths;
}

/**
 * The title and headings of a Markdown source.
 *
 * @param {string} source
 * @returns {PageOutline}
 */
export function outlineOf(source) {
    const tree = fromMarkdown(stripFrontmatter(source));
    return {
        title: frontmatterTitle(source) ?? '',
        headings: headingsOf(tree),
        depths: headingDepths(tree),
    };
}

/**
 * The anchor of the heading in `to` that sits where `anchor` sits in `from`:
 * the English page's heading and its translation's. Only when both pages lay
 * their headings out alike (the same depths, in the same order); otherwise,
 * or when `from` has no such heading, null.
 *
 * @param {string} anchor
 * @param {PageOutline} from
 * @param {PageOutline} to
 * @returns {string | null}
 */
export function mapAnchor(anchor, from, to) {
    const index = [...from.headings.keys()].indexOf(anchor);
    if (index === -1 || !from.depths || !to.depths) return null;
    const alike =
        from.depths.length === to.depths.length &&
        from.depths.every((depth, i) => depth === to.depths[i]);
    return alike ? ([...to.headings.keys()][index] ?? null) : null;
}

/**
 * Where a site link on a page in `locale` points. Off any locale (English),
 * and for a link that is not to a docs page or that names another locale, the
 * link as written. Otherwise the page under the locale's prefix, where
 * Starlight serves its translation or, failing one, the English page with
 * its fallback notice. An anchor the translation lacks maps to its heading in
 * the same place (`mapAnchor`); one that cannot map goes to the English page,
 * which has it. A same-page anchor maps the same way from the page's English
 * original. A link that resolves nowhere is returned as written, for
 * `labelParts` to report.
 *
 * @param {string} url `/route/`, `/route/#anchor` or `#anchor`.
 * @param {object} context
 * @param {string | undefined} context.locale The page's locale key.
 * @param {(route: string) => PageOutline | null} context.ownOutlineFor
 *   Outline of the file that serves exactly `route`, no fallback.
 * @param {PageOutline} context.here The page the link is on.
 * @param {PageOutline | null} context.original The English page this page
 *   translates, or null.
 * @returns {string}
 */
export function localizeUrl(url, { locale, ownOutlineFor, here, original }) {
    if (!locale) return url;
    const hashAt = url.indexOf('#');
    const route = hashAt === -1 ? url : url.slice(0, hashAt);
    const hash = hashAt === -1 ? '' : url.slice(hashAt);
    const { anchor } = splitUrl(url);

    if (route === '') {
        if (!anchor || here.headings.has(anchor) || !original) return url;
        const mapped = mapAnchor(anchor, original, here);
        return mapped === null ? url : `#${mapped}`;
    }

    const { locale: linked, path } = splitLocale(route);
    if (linked !== undefined && linked !== locale) return url;
    const english = ownOutlineFor(path);
    if (!english) return url;
    const target = localizePath(path, locale);
    const translated = ownOutlineFor(target);
    if (!translated || !anchor || translated.headings.has(anchor)) {
        return `${target}${hash}`;
    }
    const mapped = mapAnchor(anchor, english, translated);
    if (mapped !== null) return `${target}#${mapped}`;
    return english.headings.has(anchor) ? `${path}${hash}` : url;
}

/**
 * What an empty link to `url` names: the target page's title (null for a
 * heading on the same page) and the heading (null for a whole page).
 *
 * @param {string} url `/route/`, `/route/#anchor` or `#anchor`.
 * @param {(route: string) => PageOutline | null} outlineFor Outline of the
 *   page serving a route, or null when none does.
 * @param {PageOutline} here The page the link is on.
 * @returns {{ page: string | null, heading: string | null }}
 * @throws {Error} When the target page or anchor does not exist.
 */
export function labelParts(url, outlineFor, here) {
    const { route, anchor } = splitUrl(url);
    const page = route === '' ? here : outlineFor(route);
    if (!page) throw new Error(`no page serves ${route}`);
    if (!anchor) {
        if (!page.title) throw new Error(`${route} has no title`);
        return { page: page.title, heading: null };
    }
    const heading = page.headings.get(anchor);
    if (heading === undefined) {
        throw new Error(`${route || 'this page'} has no heading #${anchor}`);
    }
    return { page: route === '' ? null : page.title, heading };
}

/**
 * The label an empty link to `url` renders, as plain text.
 *
 * @param {string} url
 * @param {(route: string) => PageOutline | null} outlineFor
 * @param {PageOutline} here
 * @returns {string}
 * @throws {Error} When the target page or anchor does not exist.
 */
export function labelFor(url, outlineFor, here) {
    const { page, heading } = labelParts(url, outlineFor, here);
    return [page, heading].filter((part) => part !== null).join(SEPARATOR);
}

/**
 * True when a page is generated reference (frontmatter `generated:`, see
 * src/content.config.ts), whose symbol links keep their text.
 *
 * @param {{ data?: any, value?: unknown }} file
 * @returns {boolean}
 */
export function isGeneratedPage(file) {
    const frontmatter = file.data?.astro?.frontmatter;
    if (frontmatter) return frontmatter.generated != null;
    const block = /^---\r?\n([\s\S]*?)\r?\n---/.exec(String(file.value ?? ''));
    return block !== null && /^generated:/m.test(block[1]);
}

/** @param {string} url */
function splitUrl(url) {
    const hash = url.indexOf('#');
    if (hash === -1) return { route: url, anchor: '' };
    const raw = url.slice(hash + 1);
    try {
        return { route: url.slice(0, hash), anchor: decodeURIComponent(raw) };
    } catch {
        throw new Error(`#${raw} is not a valid percent-encoded fragment`);
    }
}

/** True for a link to this site: an in-page anchor or a site-absolute path. */
function isSiteLink(node) {
    const url = node.url ?? '';
    return (
        url.startsWith('#') || (url.startsWith('/') && !url.startsWith('//'))
    );
}

/** A text node rendered as a span of `className`. */
function part(className, value) {
    return {
        type: 'text',
        value,
        data: { hName: 'span', hProperties: { className: [className] } },
    };
}

/** The children of a labelled link: the page, separator and heading. */
function labelNodes({ page, heading }) {
    if (page === null) return [part('xref-heading', heading)];
    if (heading === null) return [part('xref-page', page)];
    return [
        part('xref-page', page),
        part('xref-sep', SEPARATOR),
        part('xref-heading', heading),
    ];
}

/**
 * @param {{ contentDir: string }} options `contentDir` is the docs collection
 *   directory, `src/content/docs`.
 */
export default function remarkLinkTitles({ contentDir }) {
    /** @type {Map<string, PageOutline>} Keyed by content file. */
    const files = new Map();
    const outlineOfFile = (rel) => {
        if (rel === null) return null;
        if (!files.has(rel)) {
            files.set(
                rel,
                outlineOf(readFileSync(join(contentDir, rel), 'utf8')),
            );
        }
        return files.get(rel);
    };
    /** The page Starlight renders at `route`, a locale's fallback included. */
    const outlineFor = (route) =>
        outlineOfFile(servingFileForRoute(contentDir, route));
    /** The file at exactly `route`: a locale's own translation, no fallback. */
    const ownOutlineFor = (route) =>
        outlineOfFile(contentFileForRoute(contentDir, route));

    /** True when a site link's target is a docs page (or this page). */
    const isDocsTarget = (url) => {
        // The route alone: the fragment does not decide whether it is a page.
        const [route] = url.split('#');
        return route === '' || outlineFor(route) !== null;
    };

    return (tree, file) => {
        const where = file.path ?? 'a docs page';
        const generated = isGeneratedPage(file);
        // The page's locale and the English page it translates, from where
        // it sits in the collection.
        const rel = file.path
            ? (isAbsolute(file.path)
                  ? relative(contentDir, file.path)
                  : file.path
              )
                  .split(sep)
                  .join('/')
            : '';
        const { locale, path: originalRel } = splitLocale(rel);
        let here = null;
        let original;
        visit(tree, 'link', (node) => {
            if (!isSiteLink(node)) return;
            const text = textOf(node).trim();
            if (text !== '') {
                if (generated || !isDocsTarget(node.url)) return;
                throw new Error(
                    `${where}: [${text}](${node.url}) has its own text; write it [](${node.url}) so it renders its target's title (development/visual-language.md)`,
                );
            }
            here ??= {
                title:
                    file.data?.astro?.frontmatter?.title ??
                    frontmatterTitle(String(file.value ?? '')) ??
                    '',
                headings: headingsOf(tree),
                depths: headingDepths(tree),
            };
            original ??=
                locale === undefined
                    ? null
                    : outlineOfFile(
                          existsSync(join(contentDir, originalRel))
                              ? originalRel
                              : null,
                      );
            const written = node.url;
            try {
                node.url = localizeUrl(written, {
                    locale,
                    ownOutlineFor,
                    here,
                    original,
                });
                node.children = labelNodes(
                    labelParts(node.url, outlineFor, here),
                );
            } catch (error) {
                throw new Error(
                    `${where}: empty link [](${written}) cannot be labelled: ${error.message}`,
                );
            }
            node.data = {
                ...node.data,
                hProperties: {
                    ...node.data?.hProperties,
                    className: [
                        ...[node.data?.hProperties?.className ?? []].flat(),
                        'xref',
                    ],
                },
            };
        });
    };
}
