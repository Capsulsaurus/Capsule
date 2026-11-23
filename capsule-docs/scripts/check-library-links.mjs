/**
 * Library-link check.
 *
 * The build links the first mention of every registered library to its source
 * repository, drawn as a badge (src/lib/remark-library-links.mjs, with the
 * registry in src/lib/library-links.mjs). The plugin never touches text that
 * is already a link, so a hand-written link to a registered repository defeats
 * it twice over: that mention renders as an ordinary link, and the badge turns
 * up on a later mention instead. `design/api-surfaces.md` shipped exactly that
 * for Kynos.
 *
 * A finding is any published occurrence of a registered repository's URL in a
 * site page: an inline or reference link, an autolink, a bare URL or an HTML
 * `href`. Code (fenced, indented or inline), comments and frontmatter are not
 * published prose and are skipped. A deeper link into a repository (a file, an issue, a
 * release) is a citation the badge cannot make, so only the repository root
 * counts, with or without a trailing slash, `.git`, `www.`, a query or a
 * fragment.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { LIBRARIES } from '../src/lib/library-links.mjs';
import {
    stripComments,
    stripFences,
    stripFrontmatter,
} from './lib/markdown.mjs';
import { walkFiles } from './lib/walk.mjs';

const SITE_CONTENT = 'capsule-docs/src/content/docs/';

/** @param {string} text */
function escapeRegExp(text) {
    return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * One pattern per registered repository, matching its root URL and nothing
 * deeper. The repository name may not continue (`kynos-cli` and `kynos.rs` are
 * other repos) and any further path segment makes it a deep link. A bare `.`
 * after the URL ends the sentence, not the name.
 *
 * @param {import('../src/lib/library-links.mjs').Library[]} libraries
 * @returns {{ url: string, pattern: RegExp }[]}
 */
export function compileUrls(libraries) {
    return libraries.map(({ url }) => {
        const rest = escapeRegExp(url.replace(/^https?:\/\/(?:www\.)?/, ''));
        return {
            url,
            pattern: new RegExp(
                `https?://(?:www\\.)?${rest}(?:\\.git)?/?(?![\\w/-]|\\.\\w)`,
                'gi',
            ),
        };
    });
}

/**
 * Blank inline code spans, keeping every column, so a URL quoted as code is
 * not read as a link.
 *
 * @param {string} line
 */
function stripInlineCode(line) {
    return line.replace(/(`+)[^`]*?\1/g, (span) => ' '.repeat(span.length));
}

const LIST_ITEM = /^([ \t]*)([-*+]|\d{1,9}[.)])([ \t]+|$)/;

/** @param {string} prefix Leading whitespace, a tab counting as four. */
function width(prefix) {
    return prefix.replace(/\t/g, '    ').length;
}

/**
 * Blank indented code blocks, keeping every line, so a URL in one is not read
 * as a link. A block is a run of lines indented four columns past the
 * enclosing list item's content (or the margin, outside a list), opened after
 * a blank line, since an indented line cannot interrupt a paragraph. A list
 * ends at the first unindented line after a blank one. Run it after
 * `stripFences`, whose blanked lines read as blank here.
 *
 * @param {string} body
 * @returns {string}
 */
export function stripIndentedCode(body) {
    let afterBlank = true;
    let inCode = false;
    /** @type {number | null} */
    let listIndent = null;
    return body
        .split('\n')
        .map((line) => {
            if (line.trim() === '') {
                afterBlank = true;
                return line;
            }
            const indent = width(/^[ \t]*/.exec(line)[0]);
            if (indent >= (listIndent ?? 0) + 4 && (inCode || afterBlank)) {
                inCode = true;
                afterBlank = false;
                return '';
            }
            inCode = false;
            const item = LIST_ITEM.exec(line);
            if (item) {
                listIndent = width(item[1]) + item[2].length + 1;
            } else if (indent === 0 && afterBlank) {
                listIndent = null;
            }
            afterBlank = false;
            return line;
        })
        .join('\n');
}

/**
 * Every hand-written link to a registered repository in one Markdown source.
 *
 * @param {string} source
 * @param {{ url: string, pattern: RegExp }[]} urls From `compileUrls`.
 * @param {{ indentedCode?: boolean }} [options] `indentedCode: false` for MDX,
 *   which has no indented code blocks.
 * @returns {{ line: number, url: string }[]}
 */
export function handWrittenLinks(source, urls, { indentedCode = true } = {}) {
    const unfenced = stripFences(stripComments(stripFrontmatter(source)));
    const body = indentedCode ? stripIndentedCode(unfenced) : unfenced;
    const found = [];
    body.split('\n').forEach((raw, index) => {
        const line = stripInlineCode(raw);
        for (const { url, pattern } of urls) {
            pattern.lastIndex = 0;
            for (const _ of line.matchAll(pattern)) {
                found.push({ line: index + 1, url });
            }
        }
    });
    return found;
}

/**
 * @param {string} root Absolute repo root.
 * @param {import('../src/lib/library-links.mjs').Library[]} [libraries]
 * @returns {{ findings: string[], checked: number }}
 */
export function checkLibraryLinks(root, libraries = LIBRARIES) {
    const urls = compileUrls(libraries);
    const pages = walkFiles(
        root,
        (rel) => rel.startsWith(SITE_CONTENT) && /\.mdx?$/.test(rel),
    );
    const findings = [];
    for (const page of pages) {
        const source = readFileSync(join(root, page), 'utf8');
        const options = { indentedCode: !page.endsWith('.mdx') };
        for (const { line, url } of handWrittenLinks(source, urls, options)) {
            findings.push(
                `${page}:${line}  ${url}  write the library's name; the build links it`,
            );
        }
    }
    return { findings, checked: pages.length };
}

/** @param {{ findings: string[], checked: number }} result */
export function reportLibraryLinks({ findings, checked }) {
    if (findings.length === 0) {
        return `library-links: ${checked} page(s), no hand-written link to a registered library.`;
    }
    return [
        `library-links: ${findings.length} hand-written link(s) to a registered library (src/lib/library-links.mjs).`,
        '',
        ...findings.map((f) => `  ${f}`),
        '',
        `library-links failed: ${findings.length} finding(s) in ${checked} page(s).`,
    ].join('\n');
}

export const libraryLinksCheck = {
    name: 'library-links',
    run: checkLibraryLinks,
    report: reportLibraryLinks,
};
