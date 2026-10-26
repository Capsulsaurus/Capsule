// Formatting region for the web client.
//
// Language and region are two settings (see the i18n design doc's Language and
// Region section): the language picks the catalog, the region drives how dates,
// times and numbers are formatted. Region never changes which strings are shown,
// and language never changes the formatting conventions beyond the words a
// formatter spells out (month and weekday names).

/** A stored preference: the key in `Storage`, absent meaning "follow the OS". */
export const REGION_STORAGE_KEY = 'capsule.region';

/** The subset of `Storage` the preference helpers need, so tests pass a fake. */
export type PreferenceStorage = Pick<
    Storage,
    'getItem' | 'setItem' | 'removeItem'
>;

/** A regular ISO 3166-1 alpha-2 region code, upper-case. */
const REGION_CODE = /^[A-Z]{2}$/;

/**
 * Two-letter codes that are not countries or territories a user lives in:
 * CLDR's private-use and pseudo-locale ranges (`AA`, `Q*`, `X*`, `ZZ`) and
 * the grouping codes (`EU`, `EZ`, `UN`).
 */
function isUserRegion(code: string): boolean {
    if (!REGION_CODE.test(code)) {
        return false;
    }
    if (code === 'AA' || code === 'ZZ') {
        return false;
    }
    if (code.startsWith('Q') || code.startsWith('X')) {
        return false;
    }
    return !['EU', 'EZ', 'UN'].includes(code);
}

/** Normalize a candidate region code, or `undefined` when it is not one. */
export function normalizeRegion(
    code: string | null | undefined,
): string | undefined {
    if (!code) {
        return undefined;
    }
    const upper = code.trim().toUpperCase();
    return isUserRegion(upper) ? upper : undefined;
}

/**
 * The region the OS reports: the first preferred tag that names a region, else
 * the likely region of the first tag (`en` maximizes to `US`). `undefined` when
 * the browser reports nothing usable.
 */
export function systemRegion(
    preferred: readonly string[] = navigator.languages,
): string | undefined {
    for (const tag of preferred) {
        try {
            const region = normalizeRegion(new Intl.Locale(tag).region);
            if (region) {
                return region;
            }
        } catch {
            // A malformed tag from the browser is skipped, not fatal.
        }
    }
    const first = preferred[0];
    if (!first) {
        return undefined;
    }
    try {
        return normalizeRegion(new Intl.Locale(first).maximize().region);
    } catch {
        return undefined;
    }
}

/**
 * The locale every `Intl` formatter is given: the UI language with the region
 * substituted in. `zh-Hant` with `GB` becomes `zh-Hant-GB` — Chinese month names,
 * British day-month order and 24-hour time. Without a region the language
 * alone is used.
 */
export function formattingLocale(
    language: string,
    region: string | undefined,
): string {
    const normalized = normalizeRegion(region);
    try {
        return new Intl.Locale(
            language,
            normalized ? { region: normalized } : {},
        ).toString();
    } catch {
        return language;
    }
}

/** The stored region override, or `undefined` when the user follows the OS. */
export function readRegionPreference(
    storage: PreferenceStorage | undefined,
): string | undefined {
    try {
        return normalizeRegion(storage?.getItem(REGION_STORAGE_KEY));
    } catch {
        // Storage can throw when the browser blocks it; treat as "follow the OS".
        return undefined;
    }
}

/** Store a region override, or clear it (`undefined`) to follow the OS again. */
export function writeRegionPreference(
    storage: PreferenceStorage | undefined,
    region: string | undefined,
): void {
    const normalized = normalizeRegion(region);
    try {
        if (normalized) {
            storage?.setItem(REGION_STORAGE_KEY, normalized);
        } else {
            storage?.removeItem(REGION_STORAGE_KEY);
        }
    } catch {
        // A blocked store keeps the choice for this page load only.
    }
}

/** The effective region: the stored override, else the OS's. */
export function resolveRegion(
    storage: PreferenceStorage | undefined,
    preferred: readonly string[] = navigator.languages,
): string | undefined {
    return readRegionPreference(storage) ?? systemRegion(preferred);
}

/** A pickable region with its name in the display language. */
export interface RegionOption {
    code: string;
    name: string;
}

const LETTERS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';

/**
 * Every region the runtime's CLDR data can name, labelled and sorted in
 * `displayLanguage`. Built from the platform rather than a hand-kept list, so it
 * follows the browser's own region data.
 */
export function regionOptions(displayLanguage: string): RegionOption[] {
    const names = new Intl.DisplayNames([displayLanguage], {
        type: 'region',
        fallback: 'none',
    });
    const options: RegionOption[] = [];
    for (const first of LETTERS) {
        for (const second of LETTERS) {
            const code = first + second;
            if (!isUserRegion(code)) {
                continue;
            }
            let name: string | undefined;
            try {
                name = names.of(code);
            } catch {
                name = undefined;
            }
            if (name && name !== code) {
                options.push({ code, name });
            }
        }
    }
    const collator = new Intl.Collator(displayLanguage);
    return options.sort((a, b) => collator.compare(a.name, b.name));
}
