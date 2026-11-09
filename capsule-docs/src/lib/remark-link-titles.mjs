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
// A label renders as `a.xref` holding `span.xref-page`, `span.xref-sep` and
// `span.xref-heading`, styled in global.css (Cross-references). The
// convention is a contract of the Visual Language doc
// (development/visual-language.md).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import GithubSlugger from 'github-slugger';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString as textOf } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';
import { stripFrontmatter } from '../../scripts/lib/markdown.mjs';
import { contentFileForRoute } from '../../scripts/lib/routes.mjs';

/** Separates a page title from a heading on another page. */
export const SEPARATOR = ' — ';

/**
 * A page's title and its headings by anchor, in document order.
 *
 * @typedef {object} PageOutline
 * @property {string} title
 * @property {Map<string, string>} headings Anchor slug to heading text.
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
 * The title and headings of a Markdown source.
 *
 * @param {string} source
 * @returns {PageOutline}
 */
export function outlineOf(source) {
    return {
        title: frontmatterTitle(source) ?? '',
        headings: headingsOf(fromMarkdown(stripFrontmatter(source))),
    };
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
    return {
        route: hash === -1 ? url : url.slice(0, hash),
        anchor: hash === -1 ? '' : decodeURIComponent(url.slice(hash + 1)),
    };
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
    /** @type {Map<string, PageOutline | null>} */
    const cache = new Map();
    const outlineFor = (route) => {
        if (!cache.has(route)) {
            const rel = contentFileForRoute(contentDir, route);
            cache.set(
                route,
                rel === null
                    ? null
                    : outlineOf(readFileSync(join(contentDir, rel), 'utf8')),
            );
        }
        return cache.get(route);
    };

    /** True when a site link's target is a docs page (or this page). */
    const isDocsTarget = (url) => {
        const { route } = splitUrl(url);
        return route === '' || outlineFor(route) !== null;
    };

    return (tree, file) => {
        const where = file.path ?? 'a docs page';
        const generated = isGeneratedPage(file);
        let here = null;
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
            };
            try {
                node.children = labelNodes(
                    labelParts(node.url, outlineFor, here),
                );
            } catch (error) {
                throw new Error(
                    `${where}: empty link [](${node.url}) cannot be labelled: ${error.message}`,
                );
            }
            node.data = {
                ...node.data,
                hProperties: {
                    ...node.data?.hProperties,
                    className: ['xref'],
                },
            };
        });
    };
}
