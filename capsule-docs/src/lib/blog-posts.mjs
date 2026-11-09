// Whether the blog has anything to show. Until at least one post is published,
// astro.config.mjs leaves the blog plugin out entirely (no /blog/ route, header
// link, RSS feed or sitemap entry) and the footer omits its link, so a reader
// never lands on an empty index. Read at config time, before content
// collections exist, so it parses frontmatter directly.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

/**
 * True when a post's frontmatter marks it `draft: true`, which Starlight
 * excludes from production builds.
 *
 * @param {string} source
 * @returns {boolean}
 */
export function isDraftPost(source) {
    const block = /^---\r?\n([\s\S]*?)\r?\n---(\r?\n|$)/.exec(source);
    return (
        block !== null && /^draft:[ \t]*true[ \t]*(#.*)?\r?$/m.test(block[1])
    );
}

/**
 * True when `dir` holds at least one Markdown post that is not a draft.
 *
 * @param {string} dir Absolute path of the blog content directory.
 * @returns {boolean}
 */
export function hasPublishedPost(dir) {
    let files;
    try {
        files = readdirSync(dir, { recursive: true, encoding: 'utf8' });
    } catch {
        return false;
    }
    return files
        .filter((file) => /\.mdx?$/.test(file))
        .some((file) => !isDraftPost(readFileSync(join(dir, file), 'utf8')));
}

/**
 * Blog authors, referenced by key from a post's `authors` frontmatter. Each
 * picture is the author's GitHub avatar, loaded from GitHub's avatar host: the
 * docs site is outside the no-third-party-asset rule
 * (development/visual-language.md).
 */
export const BLOG_AUTHORS = {
    justin: {
        name: 'Justin Chung',
        title: 'Creator & maintainer',
        url: 'https://github.com/justin13888',
        picture: 'https://avatars.githubusercontent.com/justin13888?s=96',
    },
};

/** Bounds on a post's `description`, which feeds search snippets and RSS. */
export const DESCRIPTION_LENGTH = { min: 50, max: 160 };

/** Marks where a post's excerpt ends on the blog index. */
export const EXCERPT_MARKER = '<!-- excerpt -->';

/**
 * Reads a post's top-level frontmatter fields. Handles the forms posts use:
 * `key: value`, `key: [a, b]` and a block list of `- item` lines. Scalars are
 * unquoted; lists become arrays.
 *
 * @param {string} source
 * @returns {Record<string, string | string[]>}
 */
export function postFrontmatter(source) {
    const block = /^---\r?\n([\s\S]*?)\r?\n---(\r?\n|$)/.exec(source);
    /** @type {Record<string, string | string[]>} */
    const fields = {};
    if (!block) return fields;
    let listKey = null;
    for (const line of block[1].split(/\r?\n/)) {
        const item = /^\s+-\s+(.*)$/.exec(line);
        if (item && listKey) {
            /** @type {string[]} */ (fields[listKey]).push(unquote(item[1]));
            continue;
        }
        const field = /^([A-Za-z][\w-]*):[ \t]*(.*?)[ \t]*(#.*)?$/.exec(line);
        if (!field) continue;
        const [, key, value] = field;
        listKey = null;
        if (value === '') {
            fields[key] = [];
            listKey = key;
        } else if (value.startsWith('[') && value.endsWith(']')) {
            fields[key] = value
                .slice(1, -1)
                .split(',')
                .map((part) => unquote(part.trim()))
                .filter(Boolean);
        } else {
            fields[key] = unquote(value);
        }
    }
    return fields;
}

/** @param {string} value */
function unquote(value) {
    return value.replace(/^(['"])(.*)\1$/, '$2');
}

/**
 * What a post lacks before it can be published, as readable messages; empty
 * when it meets the contract in development/visual-language.md (Blog).
 *
 * @param {string} source
 * @param {Record<string, unknown>} authors The configured authors, by key.
 * @returns {string[]}
 */
export function postContractViolations(source, authors) {
    const fields = postFrontmatter(source);
    const violations = [];
    const text = (key) =>
        typeof fields[key] === 'string'
            ? /** @type {string} */ (fields[key])
            : '';
    const list = (key) => {
        const value = fields[key];
        if (Array.isArray(value)) return value;
        return value ? [value] : [];
    };

    if (!text('title')) violations.push('missing title');
    const { min, max } = DESCRIPTION_LENGTH;
    const description = text('description');
    if (description.length < min || description.length > max) {
        violations.push(
            `description must be ${min}-${max} characters (has ${description.length})`,
        );
    }
    if (!/^\d{4}-\d{2}-\d{2}/.test(text('date'))) {
        violations.push('missing date (YYYY-MM-DD)');
    }
    const postAuthors = list('authors');
    if (postAuthors.length === 0) violations.push('missing authors');
    for (const key of postAuthors) {
        if (!Object.hasOwn(authors, key)) {
            violations.push(`unknown author "${key}"`);
        }
    }
    if (list('tags').length === 0) violations.push('missing tags');
    if (!source.includes(EXCERPT_MARKER)) {
        violations.push(`missing excerpt marker ${EXCERPT_MARKER}`);
    }
    return violations;
}
