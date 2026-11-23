// The front page's hero buttons on a locale's routes. Starlight renders a
// hero action's `link` as written, so a translated front page (or the English
// one served as a locale's fallback) would send its reader back to the English
// site. This keeps every site link in the page's locale, where Starlight
// serves the target translated or in English with its fallback notice. Used by
// src/route-data.ts.

import { localizePath } from './locales.mjs';

/**
 * A copy of `hero` whose actions' site links are under `locale`, or `hero`
 * itself when there is nothing to change. Never mutates its input: on a
 * fallback route the hero belongs to the English page's entry.
 *
 * @template {{ actions?: { link: string }[] } | undefined} H
 * @param {H} hero
 * @param {string | undefined} locale
 * @returns {H}
 */
export function localizeHero(hero, locale) {
    if (!hero?.actions?.length || !locale) return hero;
    return {
        ...hero,
        actions: hero.actions.map((action) => ({
            ...action,
            link: localizePath(action.link, locale),
        })),
    };
}
