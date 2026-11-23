// The version switcher's decisions, kept out of the component so they are
// tested: what the menu lists, where keyboard focus moves, and which side of
// the button the menu opens on. The component (VersionSelect.astro) only
// renders these and wires the events. Pure functions.
import { releasedVersions, switchTarget } from './docs-version.mjs';

/**
 * @typedef {object} MenuEntry
 * @property {string} label `Latest` or `vX.Y`.
 * @property {string} base The version's base path, its home page.
 * @property {string} href The page being read, in that version.
 * @property {boolean} current Whether it is the version being read.
 */

/**
 * The button's label for the version being read.
 *
 * @param {string} current `X.Y`, or `''` for the latest docs.
 * @returns {string}
 */
export function currentLabel(current) {
    return current === '' ? 'Latest' : `v${current}`;
}

/**
 * The menu: Latest, then every released version newest first, each linking
 * the page being read in that version. Empty while nothing is released, so
 * the switcher stays a label. A release build lists itself even before its
 * release reaches `/versions.json`.
 *
 * @param {unknown} json The parsed `/versions.json`.
 * @param {string} current `X.Y`, or `''` for the latest docs.
 * @param {string} pathname The page being read.
 * @param {string} fromBase This build's base.
 * @returns {MenuEntry[]}
 */
export function menuEntries(json, current, pathname, fromBase) {
    let versions = releasedVersions(json);
    if (current !== '' && !versions.includes(current)) {
        versions = releasedVersions({ versions: [...versions, current] });
    }
    if (versions.length === 0) return [];
    /** @param {string} version */
    const entry = (version) => {
        const base = version === '' ? '/' : `/v/${version}/`;
        return {
            label: currentLabel(version),
            base,
            href: switchTarget(pathname, fromBase, base),
            current: version === current,
        };
    };
    return [entry(''), ...versions.map(entry)];
}

/**
 * The `sessionStorage` key holding `/versions.json` once a page has read it,
 * so the next page in the tab builds its menu without waiting on the network
 * and the pill does not change shape after every navigation. Each page
 * refreshes it in the background (`versionsForMenu`), so a release published
 * mid-session shows from the next navigation.
 */
export const VERSIONS_CACHE_KEY = 'capsule-docs-versions';

/**
 * The cached `/versions.json`, or `undefined` when there is none, it does not
 * parse, or storage is unavailable.
 *
 * @param {Pick<Storage, 'getItem'> | null | undefined} storage
 * @returns {unknown}
 */
export function readCachedVersions(storage) {
    try {
        const text = storage?.getItem(VERSIONS_CACHE_KEY);
        return text == null ? undefined : JSON.parse(text);
    } catch {
        return undefined;
    }
}

/**
 * Caches `/versions.json` for the session. Storage that is full or refused
 * (a private window) leaves the next page to fetch it again.
 *
 * @param {Pick<Storage, 'setItem'> | null | undefined} storage
 * @param {unknown} json
 */
export function writeCachedVersions(storage, json) {
    try {
        storage?.setItem(VERSIONS_CACHE_KEY, JSON.stringify(json));
    } catch {
        // Not cached: the next page fetches.
    }
}

/**
 * The `/versions.json` to build the menu from, stale-while-revalidate. With a
 * cached copy it resolves to that at once and refreshes the cache from
 * `fetchList` in the background, keeping the stale copy if the fetch fails.
 * With none it waits for the fetch, caches it, and resolves to `undefined`
 * when the fetch fails. Either way the fetch starts once.
 *
 * @param {Pick<Storage, 'getItem' | 'setItem'> | null | undefined} storage
 * @param {() => Promise<unknown>} fetchList Fetches and parses the list;
 *   rejects on any failure.
 * @returns {Promise<unknown>}
 */
export async function versionsForMenu(storage, fetchList) {
    const cached = readCachedVersions(storage);
    const refresh = fetchList().then((json) => {
        writeCachedVersions(storage, json);
        return json;
    });
    if (cached !== undefined) {
        refresh.catch(() => {
            // The stale copy stays until a later page's refresh succeeds.
        });
        return cached;
    }
    try {
        return await refresh;
    } catch {
        return undefined;
    }
}

/**
 * Whether the switcher will become a menu, decided before its script runs so
 * the pill reserves the chevron's width from its first paint. A release build
 * always lists itself; the latest build does once the cached list names a
 * release. With no cache it guesses no menu, the state until a first release.
 * Self-contained: `reserveScript` ships its source.
 *
 * @param {string} current `X.Y`, or `''` for the latest docs.
 * @param {string | null} cached The raw cached `/versions.json`.
 * @returns {boolean}
 */
export function expectsMenu(current, cached) {
    if (current !== '') return true;
    try {
        const versions = JSON.parse(cached ?? 'null')?.versions;
        return (
            Array.isArray(versions) &&
            versions.some((v) => typeof v === 'string' && /^\d+\.\d+$/.test(v))
        );
    } catch {
        return false;
    }
}

/**
 * Inline script placed straight after a `<capsule-version-select>`: marks it
 * `data-expect-menu` while the page parses, before the first paint.
 *
 * @returns {string}
 */
export function reserveScript() {
    return `(() => {
const expectsMenu = ${expectsMenu.toString()};
const el = document.currentScript.previousElementSibling;
let cached = null;
try { cached = sessionStorage.getItem(${JSON.stringify(VERSIONS_CACHE_KEY)}); } catch {}
if (expectsMenu(el.dataset.current || '', cached)) el.dataset.expectMenu = '';
})();`;
}

/**
 * The item a navigation key moves focus to: arrows step and wrap, Home and
 * End jump. `index` is -1 when focus is outside the list. `null` for any
 * other key.
 *
 * @param {string} key `KeyboardEvent.key`
 * @param {number} index
 * @param {number} count
 * @returns {number | null}
 */
export function moveFocus(key, index, count) {
    if (count === 0) return null;
    switch (key) {
        case 'ArrowDown':
            return (index + 1) % count;
        case 'ArrowUp':
            return index <= 0 ? count - 1 : index - 1;
        case 'Home':
            return 0;
        case 'End':
            return count - 1;
        default:
            return null;
    }
}

/**
 * Which side of the button the menu opens on: below when it fits, above
 * when only that side fits (the narrow-screen menu puts the switcher at the
 * bottom of the window), otherwise the roomier side.
 *
 * @param {number} below Free height below the button, in pixels.
 * @param {number} above Free height above it.
 * @param {number} height The menu's height.
 * @returns {'below' | 'above'}
 */
export function placement(below, above, height) {
    if (below >= height) return 'below';
    if (above >= height) return 'above';
    return above > below ? 'above' : 'below';
}
