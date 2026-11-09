/**
 * Astro integration: put a release build's site-absolute links under its base.
 *
 * Astro prefixes its own assets and Starlight its navigation with the build's
 * `base` (`/v/X.Y/` for a release; src/lib/docs-version.mjs), but a link
 * written anywhere else (Markdown, MDX, the hero's raw HTML, a component's
 * hard-coded path) is left as written, and on a release build it would leave
 * the version for the latest docs. So once the build is done, every `href`,
 * `src` and `srcset` in every HTML page goes through `withBase`, which leaves
 * external, relative, shared-root and already-based links alone. On the
 * latest build, whose base is `/`, it does nothing.
 */

import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { rehype } from 'rehype';
import { visit } from 'unist-util-visit';
import { withBase } from './docs-version.mjs';

/**
 * `html` with its site-absolute links under `base`.
 *
 * @param {string} html A whole document.
 * @param {string} base
 * @returns {string}
 */
export function rebaseHtml(html, base) {
    if (base === '/') return html;
    const processor = rehype().use(() => (tree) => {
        visit(tree, 'element', (node) => {
            const props = node.properties ?? {};
            for (const name of ['href', 'src']) {
                if (typeof props[name] === 'string') {
                    props[name] = withBase(props[name], base);
                }
            }
            // A srcset is comma-separated candidates, each a URL and a size.
            if (typeof props.srcSet === 'string') {
                props.srcSet = props.srcSet
                    .split(',')
                    .map((candidate) => {
                        const [url, ...size] = candidate.trim().split(/\s+/);
                        return [withBase(url, base), ...size].join(' ');
                    })
                    .join(', ');
            }
        });
    });
    return String(processor.processSync(html));
}

/**
 * @param {{ base: string }} options
 * @returns {import('astro').AstroIntegration}
 */
export default function baseLinks({ base }) {
    return {
        name: 'capsule-base-links',
        hooks: {
            'astro:build:done': ({ dir }) => {
                if (base === '/') return;
                const root = fileURLToPath(dir);
                const pages = readdirSync(root, {
                    recursive: true,
                    encoding: 'utf8',
                }).filter((path) => path.endsWith('.html'));
                for (const page of pages) {
                    const file = join(root, page);
                    writeFileSync(
                        file,
                        rebaseHtml(readFileSync(file, 'utf8'), base),
                    );
                }
            },
        },
    };
}
