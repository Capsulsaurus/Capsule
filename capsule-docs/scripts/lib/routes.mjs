/**
 * Site route → content file, shared by the docs-truth cross-link check and the
 * markdown pipeline (src/lib/remark-link-titles.mjs). Node built-ins only: the
 * docs-truth CI job installs no dependencies.
 */

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { splitLocale } from '../../src/lib/locales.mjs';

/**
 * Resolve a site route (`/design/thumbnails/`) to the content file that
 * serves it, trying the page and then the section index, `.md` before `.mdx`.
 * A locale's route (`/ja/guides/`) resolves only to that locale's own
 * translation; see {@link servingFileForRoute} for the English fallback.
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

/**
 * The content file Starlight renders at a route: the locale's translation
 * when one exists, otherwise, for a route under a locale
 * (src/lib/locales.mjs), the English page it falls back to.
 *
 * @param {string} contentDir
 * @param {string} route
 * @returns {string | null}
 */
export function servingFileForRoute(contentDir, route) {
    const own = contentFileForRoute(contentDir, route);
    if (own !== null) return own;
    const { locale, path } = splitLocale(`/${route.replace(/^\/+/, '')}`);
    return locale === undefined ? null : contentFileForRoute(contentDir, path);
}
