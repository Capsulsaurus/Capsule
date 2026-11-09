/**
 * Site route → content file, shared by the docs-truth cross-link check and the
 * markdown pipeline (src/lib/remark-link-titles.mjs). Node built-ins only: the
 * docs-truth CI job installs no dependencies.
 */

import { existsSync } from 'node:fs';
import { join } from 'node:path';

/**
 * Resolve a site route (`/design/thumbnails/`) to the content file that
 * serves it, trying the page and then the section index, `.md` before `.mdx`.
 *
 * @param {string} contentDir Directory of the docs collection
 *   (`src/content/docs`), absolute or relative to the working directory.
 * @param {string} route Pathname, with or without surrounding slashes.
 * @returns {string | null} The file's path relative to `contentDir`, or null
 *   when no file serves the route.
 */
export function contentFileForRoute(contentDir, route) {
    const clean = route.replace(/^\/+|\/+$/g, '');
    const bases = clean === '' ? ['index'] : [clean, `${clean}/index`];
    for (const base of bases) {
        for (const ext of ['.md', '.mdx']) {
            const rel = `${base}${ext}`;
            if (existsSync(join(contentDir, rel))) return rel;
        }
    }
    return null;
}
