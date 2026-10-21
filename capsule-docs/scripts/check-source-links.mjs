/**
 * Source-comment link check.
 *
 * Code comments cite design docs two ways, and a doc rename breaks both
 * silently because neither `starlight-links-validator` nor the cross-links
 * check reads source files:
 *
 *   - a Markdown link in a doc comment, relative to the source file:
 *     `[Bounds](../../capsule-docs/src/content/docs/design/x.md#bounds)`
 *   - a bare site path in prose: `design/filesystem/server.md#sessions`
 *
 * Both must name an existing page, and an anchor must name a heading on it.
 */

import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, normalize, sep } from 'node:path';
import { headingAnchors } from './lib/markdown.mjs';
import { walkFiles } from './lib/walk.mjs';

const SITE_CONTENT = 'capsule-docs/src/content/docs';

/** Source languages whose comments cite the docs. */
const SOURCE_EXTENSIONS = ['.rs', '.swift', '.kt', '.kts', '.ts', '.tsx'];

/** Non-buildable reference material keeps the links it was written with. */
const SKIP_PREFIXES = ['legacy-review/'];

/** `](target.md#anchor)` — a Markdown link to a doc page. */
const MARKDOWN_LINK = /\]\(\s*<?([^)\s>]+\.mdx?)(?:#([^)\s>]+))?>?\s*\)/g;

/** A bare site path, not the tail of a longer path or a link target. */
const BARE_PATH =
    /(?<![\w./-])(?<!\]\()((?:design|development|guides|features)\/[\w/-]+\.mdx?)(?:#([\w-]+))?/g;

/**
 * @param {string} root Absolute repo root.
 * @returns {{ findings: string[], checked: number }}
 */
export function checkSourceLinks(root) {
    const findings = [];
    let checked = 0;

    const anchorCache = new Map();
    const anchorsOf = (relPath) => {
        if (!anchorCache.has(relPath)) {
            anchorCache.set(
                relPath,
                headingAnchors(readFileSync(join(root, relPath), 'utf8')),
            );
        }
        return anchorCache.get(relPath);
    };

    const verify = (at, shown, relPath, anchor) => {
        checked += 1;
        const abs = join(root, relPath);
        if (!existsSync(abs) || statSync(abs).isDirectory()) {
            findings.push(`${at}  ${shown}  no such page`);
            return;
        }
        if (anchor && !anchorsOf(relPath).has(anchor.toLowerCase())) {
            findings.push(`${at}  ${shown}  ${relPath} has no #${anchor}`);
        }
    };

    const sources = walkFiles(
        root,
        (rel) =>
            SOURCE_EXTENSIONS.some((ext) => rel.endsWith(ext)) &&
            !SKIP_PREFIXES.some((prefix) => rel.startsWith(prefix)),
    );

    for (const source of sources) {
        const lines = readFileSync(join(root, source), 'utf8').split('\n');
        lines.forEach((text, index) => {
            const at = `${source}:${index + 1}`;
            for (const match of text.matchAll(MARKDOWN_LINK)) {
                const [, target, anchor] = match;
                if (/^[a-z][a-z0-9+.-]*:/i.test(target)) continue;
                const resolved = normalize(join(dirname(source), target))
                    .split(sep)
                    .join('/');
                verify(at, target, resolved, anchor);
            }

            for (const match of text.matchAll(BARE_PATH)) {
                const [whole, path, anchor] = match;
                verify(at, whole, `${SITE_CONTENT}/${path}`, anchor);
            }
        });
    }

    return { findings, checked };
}

/** @param {{ findings: string[], checked: number }} result */
export function reportSourceLinks({ findings, checked }) {
    if (findings.length === 0) {
        return `source-links: ${checked} doc reference(s) in source comments, all resolve.`;
    }
    return [
        `source-links: ${findings.length} broken doc reference(s) in source comments.`,
        '',
        ...findings.map((f) => `  ${f}`),
        '',
        `source-links failed: ${findings.length} broken of ${checked} checked.`,
    ].join('\n');
}

export const sourceLinksCheck = {
    name: 'source-links',
    run: checkSourceLinks,
    report: reportSourceLinks,
};
