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
// no text to fall back to. Links with text, and links off the site, are left
// alone. The convention is a contract of the Visual Language doc
// (development/visual-language.md).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import GithubSlugger from 'github-slugger';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString } from 'mdast-util-to-string';
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
        const text = toString(node).trim();
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
 * The label an empty link to `url` renders, from the page it appears on.
 *
 * @param {string} url `/route/`, `/route/#anchor` or `#anchor`.
 * @param {(route: string) => PageOutline | null} outlineFor Outline of the
 *   page serving a route, or null when none does.
 * @param {PageOutline} here The page the link is on.
 * @returns {string}
 * @throws {Error} When the target page or anchor does not exist.
 */
export function labelFor(url, outlineFor, here) {
    const hash = url.indexOf('#');
    const route = hash === -1 ? url : url.slice(0, hash);
    const anchor = hash === -1 ? '' : decodeURIComponent(url.slice(hash + 1));

    const page = route === '' ? here : outlineFor(route);
    if (!page) throw new Error(`no page serves ${route}`);
    if (!anchor) {
        if (!page.title) throw new Error(`${route} has no title`);
        return page.title;
    }
    const heading = page.headings.get(anchor);
    if (heading === undefined) {
        throw new Error(`${route || 'this page'} has no heading #${anchor}`);
    }
    return route === '' ? heading : `${page.title}${SEPARATOR}${heading}`;
}

/** True for a link this plugin labels: on-site, and with no text. */
function isEmptySiteLink(node) {
    const url = node.url ?? '';
    const onSite =
        url.startsWith('#') || (url.startsWith('/') && !url.startsWith('//'));
    return onSite && toString(node).trim() === '';
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

    return (tree, file) => {
        let here = null;
        visit(tree, 'link', (node) => {
            if (!isEmptySiteLink(node)) return;
            here ??= {
                title:
                    file.data?.astro?.frontmatter?.title ??
                    frontmatterTitle(String(file.value ?? '')) ??
                    '',
                headings: headingsOf(tree),
            };
            try {
                node.children = [
                    {
                        type: 'text',
                        value: labelFor(node.url, outlineFor, here),
                    },
                ];
            } catch (error) {
                throw new Error(
                    `${file.path ?? 'a docs page'}: empty link [](${node.url}) cannot be labelled: ${error.message}`,
                );
            }
        });
    };
}
