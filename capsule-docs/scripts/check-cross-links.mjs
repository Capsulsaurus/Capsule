/**
 * Cross-boundary link check.
 *
 * `starlightLinksValidator()` (capsule-docs/astro.config.mjs) proves every link
 * *inside* the documentation site during `mise run build-docs`. It cannot see
 * the links that cross the site boundary, and those are the ones a docs
 * restructure breaks:
 *
 *   - `SLICES.md` and `AGENTS.md` reference design docs by repo-relative path,
 *     sometimes with an anchor. Renaming a design doc breaks them silently.
 *   - The root READMEs link the published site by absolute URL. Moving a page
 *     breaks them silently.
 *   - A leading-slash link is repo-root-relative to a human but resolves to
 *     nothing on GitHub, which is where these files are read.
 *
 * The check is deliberately narrow: it proves that a link *resolves*, never
 * that the text around it is accurate.
 */

import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, normalize, resolve, sep } from 'node:path';
import { headingAnchors, linkTargets } from './lib/markdown.mjs';
import { walkFiles } from './lib/walk.mjs';

/** Where the Starlight content collection lives, relative to the repo root. */
const SITE_CONTENT = 'capsule-docs/src/content/docs';

/** Generated or duplicated files whose links are not hand-maintained. */
const SKIP_SOURCES = new Set(['CHANGELOG.md']);

/**
 * `legacy-review/` is non-buildable reference material describing trees that
 * have been deleted (`capsule-api`, the Salvo `media` and `sync` modules). Its
 * links point at what *was* there, which is the record it exists to keep;
 * "fixing" them would corrupt it. Same reasoning `SLICES.md` gets for the
 * module paths it names historically.
 */
const SKIP_SOURCE_PREFIXES = ['legacy-review/'];

/**
 * The published origin, read from the Astro config so a domain change cannot
 * silently strand this check on a stale hostname.
 *
 * @param {string} root
 * @returns {string | null}
 */
function siteOrigin(root) {
    const config = join(root, 'capsule-docs/astro.config.mjs');
    if (!existsSync(config)) return null;
    const match = /\bsite:\s*['"]([^'"]+)['"]/.exec(
        readFileSync(config, 'utf8'),
    );
    return match ? match[1].replace(/\/$/, '') : null;
}

/**
 * Resolve a site route (`/design/thumbnails/`) to the content file that serves
 * it, trying the page and then the section index, `.md` before `.mdx`.
 *
 * @param {string} root
 * @param {string} route Pathname, with or without surrounding slashes.
 * @returns {string | null} Repo-relative content path, or null when unserved.
 */
function contentFileForRoute(root, route) {
    const clean = route.replace(/^\/+|\/+$/g, '');
    const bases = clean === '' ? ['index'] : [clean, `${clean}/index`];
    for (const base of bases) {
        for (const ext of ['.md', '.mdx']) {
            const rel = `${SITE_CONTENT}/${base}${ext}`;
            if (existsSync(join(root, rel))) return rel;
        }
    }
    return null;
}

/**
 * Percent-decode, falling back to the raw text. `decodeURIComponent` throws on
 * a bare `%` — `#50%-of-quota` is a legal anchor and an uncaught `URIError`
 * here would kill the whole run with a stack trace instead of a report.
 *
 * @param {string} value
 * @returns {string}
 */
function decode(value) {
    try {
        return decodeURIComponent(value);
    } catch {
        return value;
    }
}

/** True when a target is a scheme we never resolve (http to elsewhere, mailto, …). */
function isExternal(target, origin) {
    if (origin && target.startsWith(origin)) return false;
    return /^[a-z][a-z0-9+.-]*:/i.test(target) || target.startsWith('//');
}

/**
 * @param {string} root Absolute repo root.
 * @returns {{ findings: string[], checked: number }}
 */
export function checkCrossLinks(root) {
    const origin = siteOrigin(root);
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

    const sources = walkFiles(
        root,
        (rel) =>
            (rel.endsWith('.md') || rel.endsWith('.mdx')) &&
            !SKIP_SOURCES.has(rel) &&
            !SKIP_SOURCE_PREFIXES.some((prefix) => rel.startsWith(prefix)),
    );

    for (const source of sources) {
        const insideSite = source.startsWith(`${SITE_CONTENT}/`);
        const body = readFileSync(join(root, source), 'utf8');

        for (const { target, line } of linkTargets(body)) {
            const at = `${source}:${line}`;
            const [encodedPath, rawAnchor] = target.split('#');
            // The path is decoded too: `[a](my%20file.md)` names a real file.
            const rawPath = decode(encodedPath);
            const anchor = rawAnchor ? decode(rawAnchor).toLowerCase() : null;

            // Same-document anchor.
            if (rawPath === '') {
                if (insideSite) continue; // starlight-links-validator owns these
                checked += 1;
                if (anchor && !anchorsOf(source).has(anchor)) {
                    findings.push(
                        `${at}  ${target}  no heading in this file slugs to #${anchor}`,
                    );
                }
                continue;
            }

            // A published-site URL, from anywhere.
            if (origin && target.startsWith(origin)) {
                checked += 1;
                const route = rawPath.slice(origin.length) || '/';
                const file = contentFileForRoute(root, route);
                if (!file) {
                    findings.push(
                        `${at}  ${rawPath}  no page serves this route`,
                    );
                } else if (anchor && !anchorsOf(file).has(anchor)) {
                    findings.push(
                        `${at}  ${target}  ${file} has no #${anchor}`,
                    );
                }
                continue;
            }

            if (isExternal(target, origin)) continue;

            // Root-relative. Inside the site these are Starlight routes and are
            // already validated; outside it they resolve to nothing on GitHub.
            if (rawPath.startsWith('/')) {
                if (insideSite) continue;
                checked += 1;
                const rel = rawPath.replace(/^\/+/, '');
                const abs = join(root, rel);
                const hint = existsSync(abs)
                    ? 'a leading slash is not repo-relative on GitHub; use a path relative to this file'
                    : 'no such path';
                findings.push(`${at}  ${rawPath}  ${hint}`);
                continue;
            }

            // Repo-relative. Only Markdown targets are resolved: a link to a
            // source file is checked for existence, everything else is left be.
            const resolved = normalize(join(dirname(source), rawPath));
            if (resolved.startsWith('..')) {
                checked += 1;
                findings.push(`${at}  ${rawPath}  escapes the repository`);
                continue;
            }
            const relPath = resolved.split(sep).join('/');
            const abs = resolve(root, relPath);
            checked += 1;

            if (!existsSync(abs)) {
                findings.push(`${at}  ${rawPath}  no such file`);
                continue;
            }
            // A relative link to a directory is legitimate — GitHub renders it
            // as a tree view, which is what `SLICES.md` means when it links the
            // design-docs folder. There is no anchor to resolve inside one.
            if (statSync(abs).isDirectory()) continue;
            if (anchor && /\.mdx?$/.test(relPath)) {
                if (!anchorsOf(relPath).has(anchor)) {
                    findings.push(
                        `${at}  ${rawPath}#${anchor}  ${relPath} has no #${anchor}`,
                    );
                }
            }
        }
    }

    return { findings, checked };
}

/** @param {{ findings: string[], checked: number }} result */
export function reportCrossLinks({ findings, checked }) {
    if (findings.length === 0) {
        return `cross-links: ${checked} link(s) checked, all resolve.`;
    }
    const lines = [
        `cross-links: ${findings.length} broken link(s) outside the Starlight site.`,
        '',
        ...findings.map((f) => `  ${f}`),
        '',
        'Site-internal links are validated by starlight-links-validator during `mise run build-docs`.',
        '',
        `cross-links failed: ${findings.length} broken of ${checked} checked.`,
    ];
    return lines.join('\n');
}

export const crossLinksCheck = {
    name: 'cross-links',
    run: checkCrossLinks,
    report: reportCrossLinks,
};
