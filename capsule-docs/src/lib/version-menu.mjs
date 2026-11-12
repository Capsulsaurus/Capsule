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
