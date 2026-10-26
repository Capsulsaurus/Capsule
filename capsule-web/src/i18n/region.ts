// Formatting region for the web client.
//
// Language and region are two settings (see the i18n design doc's Language and
// Region section): the language picks the catalog, the region drives how dates,
// times and numbers are formatted. Region never changes which strings are shown,
// and language never changes the formatting conventions: at most it supplies the
// words a formatter spells out (month and weekday names), where the runtime has
// data for that language in that region (see `formattingLocale`).

/** A stored preference: the key in `Storage`, absent meaning "follow the OS". */
export const REGION_STORAGE_KEY = 'capsule.region';

/** The subset of `Storage` the preference helpers need, so tests pass a fake. */
export type PreferenceStorage = Pick<
    Storage,
    'getItem' | 'setItem' | 'removeItem'
>;

/** A regular ISO 3166-1 alpha-2 region code, upper-case. */
const REGION_CODE = /^[A-Z]{2}$/;

/** A UN M.49 numeric region code (`419` Latin America, `150` Europe). */
const NUMERIC_REGION_CODE = /^\d{3}$/;

/**
 * Codes that are not countries or territories a user lives in: CLDR's
 * private-use and pseudo-locale ranges (`AA`, `Q*`, `X*`, `ZZ`) and the
 * grouping codes (`EU`, `EZ`, `UN`). A numeric M.49 code is accepted, because
 * browsers report tags such as `es-419`; which of them the picker offers is
 * decided by `regionOptions`.
 */
function isUserRegion(code: string): boolean {
    if (NUMERIC_REGION_CODE.test(code)) {
        return true;
    }
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
 * Whether the runtime has formatting data for `tag` in `region` itself, rather
 * than silently falling back to the bare language. CLDR has no `zh-Hant-GB` or
 * `fr-US`, so `Intl` formats those exactly as `zh-Hant` and `fr`; the resolved
 * locale then drops the region, which is what this detects.
 */
function hasRegionalData(tag: string, region: string): boolean {
    try {
        const resolved = new Intl.DateTimeFormat(tag).resolvedOptions().locale;
        return new Intl.Locale(resolved).region === region;
    } catch {
        return false;
    }
}

/**
 * The region's own default locale (`GB` gives `en-GB`, `TW` gives `zh-TW`,
 * `419` gives `es-419`), or `undefined` when the runtime has no formatting
 * data for any.
 */
function regionDefaultLocale(region: string): string | undefined {
    try {
        const likely = new Intl.Locale('und', { region }).maximize();
        // `en-Latn-GB` resolves to bare `en`, so the script is tried second.
        const candidates = [
            new Intl.Locale(likely.language, { region }).toString(),
            new Intl.Locale(likely.language, {
                script: likely.script,
                region,
            }).toString(),
        ];
        return candidates.find((tag) => hasRegionalData(tag, region));
    } catch {
        return undefined;
    }
}

/**
 * The locale every `Intl` formatter is given, so that the region decides how
 * dates, times and numbers are written.
 *
 * Where the runtime has data for the UI language in that region, it is the
 * language with the region substituted in: `zh-Hant` with `HK` becomes
 * `zh-Hant-HK`, `en` with `DE` becomes `en-DE`, and month names stay in the UI
 * language. Where it has none (`zh-Hant` with `GB`, `fr` with `US`), the
 * language-region pair would format exactly as the bare language and the
 * region would have no effect, so the region's own default locale is used
 * instead (`en-GB`, `en-US`): the region's date order, clock and grouping, with
 * month names in that locale's language. Without a usable region, or when the
 * runtime knows neither, the language-region pair (or the language alone) is
 * used.
 */
export function formattingLocale(
    language: string,
    region: string | undefined,
): string {
    const normalized = normalizeRegion(region);
    let paired: string;
    try {
        paired = new Intl.Locale(
            language,
            normalized ? { region: normalized } : {},
        ).toString();
    } catch {
        return language;
    }
    if (!normalized || hasRegionalData(paired, normalized)) {
        return paired;
    }
    return regionDefaultLocale(normalized) ?? paired;
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

/** Every candidate code: the alpha-2 grid, then the numeric `000`–`999` range. */
function candidateCodes(): string[] {
    const codes: string[] = [];
    for (const first of LETTERS) {
        for (const second of LETTERS) {
            codes.push(first + second);
        }
    }
    for (let number = 0; number < 1000; number += 1) {
        codes.push(String(number).padStart(3, '0'));
    }
    return codes;
}

/**
 * Every region the runtime's CLDR data can name, labelled and sorted in
 * `displayLanguage`. Built from the platform rather than a hand-kept list, so it
 * follows the browser's own region data. A numeric M.49 code is offered only
 * where the runtime has formatting data for it (`419`, `150`), so continents
 * that would format exactly as the bare language are left out.
 */
export function regionOptions(displayLanguage: string): RegionOption[] {
    const names = new Intl.DisplayNames([displayLanguage], {
        type: 'region',
        fallback: 'none',
    });
    const options: RegionOption[] = [];
    for (const code of candidateCodes()) {
        if (!isUserRegion(code)) {
            continue;
        }
        let name: string | undefined;
        try {
            name = names.of(code);
        } catch {
            name = undefined;
        }
        if (!name || name === code) {
            continue;
        }
        if (
            NUMERIC_REGION_CODE.test(code) &&
            !hasRegionalData(formattingLocale(displayLanguage, code), code)
        ) {
            continue;
        }
        options.push({ code, name });
    }
    const collator = new Intl.Collator(displayLanguage);
    return options.sort((a, b) => collator.compare(a.name, b.name));
}
