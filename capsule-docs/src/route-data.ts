// Starlight route middleware: shows each page's review status in the sidebar.
// The decoration itself is the pure, tested `badgeSidebar`; this file only
// supplies the statuses and hands the result back to Starlight.

import { getCollection } from 'astro:content';
import { defineRouteMiddleware } from '@astrojs/starlight/route-data';
import { badgeSidebar, routeKey } from './lib/sidebar-status.mjs';

let statuses: Promise<ReadonlyMap<string, string>> | undefined;

/**
 * Every docs page's `status`, keyed by route. Cached for a production build;
 * rebuilt per request in `astro dev` so editing a page's status shows at once.
 */
function statusByRoute(): Promise<ReadonlyMap<string, string>> {
    if (import.meta.env.DEV) statuses = undefined;
    statuses ??= getCollection('docs').then(
        (entries) =>
            new Map(
                entries.flatMap(({ id, data: { status } }) =>
                    status ? [[routeKey(id), status] as const] : [],
                ),
            ),
    );
    return statuses;
}

export const onRequest = defineRouteMiddleware(async (context) => {
    const route = context.locals.starlightRoute;
    route.sidebar = badgeSidebar(route.sidebar, await statusByRoute());
});
