// Starlight route middleware: shows each page's audit status in the sidebar,
// and keeps the front page's hero buttons in the page's locale. The
// decorations themselves are the pure, tested `badgeSidebar` and
// `localizeHero`; this file only supplies the statuses and hands the results
// back to Starlight.

import { getCollection } from 'astro:content';
import { defineRouteMiddleware } from '@astrojs/starlight/route-data';
import { entryAudit } from './lib/entry-audit.mjs';
import { localizeHero } from './lib/hero-links.mjs';
import { LOCALE_KEYS } from './lib/locales.mjs';
import {
    badgeSidebar,
    routeKey,
    withLocaleFallbacks,
} from './lib/sidebar-status.mjs';

let statuses: Promise<ReadonlyMap<string, string>> | undefined;

/**
 * Every docs page's computed audit status, keyed by route, with each locale's
 * fallback routes carrying the English page's. Cached for a production build;
 * rebuilt per request in `astro dev` so editing a page shows at once.
 */
function statusByRoute(): Promise<ReadonlyMap<string, string>> {
    if (import.meta.env.DEV) statuses = undefined;
    statuses ??= getCollection('docs').then((entries) =>
        withLocaleFallbacks(
            new Map(
                entries.flatMap((entry) => {
                    const audit = entryAudit(entry);
                    return audit
                        ? [[routeKey(entry.id), audit.status] as const]
                        : [];
                }),
            ),
            LOCALE_KEYS,
        ),
    );
    return statuses;
}

export const onRequest = defineRouteMiddleware(async (context) => {
    const route = context.locals.starlightRoute;
    route.sidebar = badgeSidebar(route.sidebar, await statusByRoute());
    const hero = localizeHero(route.entry.data.hero, route.locale);
    if (hero !== route.entry.data.hero) {
        // A copy: on a fallback route the entry is the English page's.
        route.entry = { ...route.entry, data: { ...route.entry.data, hero } };
    }
});
