// The header's three-way theme toggle: Auto → Light → Dark → Auto. Pure
// functions, shared by ThemeToggle.astro's script and its tests. The storage
// key and stored values match Starlight's own theme picker, so Starlight's
// ThemeProvider still applies the saved theme before first paint.

/** @typedef {'auto' | 'light' | 'dark'} ThemePreference */

/** Key in `localStorage` that Starlight reads the preference from. */
export const STORAGE_KEY = 'starlight-theme';

/** @type {readonly ThemePreference[]} */
export const CYCLE = ['auto', 'light', 'dark'];

/** @type {Record<ThemePreference, string>} */
export const LABELS = { auto: 'Auto', light: 'Light', dark: 'Dark' };

/**
 * Coerces any stored value to a preference; unknown values mean `auto`.
 *
 * @param {unknown} value
 * @returns {ThemePreference}
 */
export function parseTheme(value) {
    return value === 'light' || value === 'dark' ? value : 'auto';
}

/**
 * The preference one press of the toggle moves to.
 *
 * @param {ThemePreference} current
 * @returns {ThemePreference}
 */
export function nextTheme(current) {
    return CYCLE[(CYCLE.indexOf(parseTheme(current)) + 1) % CYCLE.length];
}

/**
 * The theme a page renders in for a preference.
 *
 * @param {ThemePreference} preference
 * @param {boolean} systemPrefersLight
 * @returns {'light' | 'dark'}
 */
export function resolveTheme(preference, systemPrefersLight) {
    if (preference === 'auto') return systemPrefersLight ? 'light' : 'dark';
    return preference;
}

/**
 * The value written to storage. Starlight stores `auto` as the empty string.
 *
 * @param {ThemePreference} preference
 * @returns {string}
 */
export function storedValue(preference) {
    return preference === 'auto' ? '' : preference;
}

/**
 * The toggle's accessible name: the current preference and what a press does.
 *
 * @param {ThemePreference} preference
 * @returns {string}
 */
export function toggleLabel(preference) {
    return `Theme: ${LABELS[preference]}. Switch to ${LABELS[nextTheme(preference)]}.`;
}
