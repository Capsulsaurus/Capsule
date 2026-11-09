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
