// Decorates Starlight's sidebar with review-status badges. Pure: it returns a
// new tree and never mutates its input. Wired in by src/route-data.ts.
import { navBadge } from './status-badge.mjs';

/**
 * Canonical key for a docs route: no leading or trailing slash, no query or
 * fragment, no trailing `index` segment, and the home page as the empty
 * string. Content-entry ids (`index`, `design/index`) and sidebar hrefs (`/`,
 * `/design/`) both normalize to the same key.
 *
 * @param {string} path
 * @returns {string}
 */
export function routeKey(path) {
    return path
        .split(/[?#]/, 1)[0]
        .replace(/^\/+|\/+$/g, '')
        .replace(/(^|\/)index$/, '');
}

/** @param {string} href */
function isExternal(href) {
    return /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('//');
}

/**
 * Return a copy of `entries` in which every internal link whose page is not
 * `stable` carries that status's badge. A link that already has a badge keeps
 * it; external links and routes with no known status are left alone; groups
 * are recursed into but never badged themselves.
 *
 * @template {{ type: string, href?: string, badge?: unknown, entries?: any[] }} E
 * @param {E[]} entries Starlight sidebar entries.
 * @param {ReadonlyMap<string, string>} statusByRoute Keyed by {@link routeKey}.
 * @returns {E[]}
 */
export function badgeSidebar(entries, statusByRoute) {
    return entries.map((entry) => {
        if (entry.type === 'group') {
            return {
                ...entry,
                entries: badgeSidebar(entry.entries ?? [], statusByRoute),
            };
        }
        if (entry.type !== 'link' || entry.badge || !entry.href) return entry;
        if (isExternal(entry.href)) return entry;
        const badge = navBadge(statusByRoute.get(routeKey(entry.href)));
        return badge ? { ...entry, badge } : entry;
    });
}
