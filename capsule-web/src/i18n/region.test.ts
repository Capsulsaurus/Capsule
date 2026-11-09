import { describe, expect, test } from 'bun:test';

import {
    formattingLocale,
    normalizeRegion,
    type PreferenceStorage,
    REGION_STORAGE_KEY,
    readRegionPreference,
    regionOptions,
    resolveRegion,
    systemRegion,
    writeRegionPreference,
} from './region';

/** An in-memory `Storage` stand-in. */
function memoryStorage(
    initial: Record<string, string> = {},
): PreferenceStorage & {
    data: Map<string, string>;
} {
    const data = new Map(Object.entries(initial));
    return {
        data,
        getItem: (key) => data.get(key) ?? null,
        setItem: (key, value) => {
            data.set(key, value);
        },
        removeItem: (key) => {
            data.delete(key);
        },
    };
}

/** A `Storage` that throws on every call, as a browser blocking storage does. */
const blockedStorage: PreferenceStorage = {
    getItem: () => {
        throw new Error('blocked');
    },
    setItem: () => {
        throw new Error('blocked');
    },
    removeItem: () => {
        throw new Error('blocked');
    },
};

describe('normalizeRegion', () => {
    test('upper-cases a two-letter code', () => {
        expect(normalizeRegion('gb')).toBe('GB');
        expect(normalizeRegion(' hk ')).toBe('HK');
    });

    test('keeps a numeric M.49 code', () => {
        expect(normalizeRegion('419')).toBe('419');
    });

    test('refuses what is not a user region', () => {
        for (const code of [
            undefined,
            null,
            '',
            'G',
            'GBR',
            '41',
            '4190',
            'ZZ',
            'XA',
            'QO',
            'EU',
            'UN',
            'AA',
        ]) {
            expect(normalizeRegion(code)).toBeUndefined();
        }
    });
});

describe('systemRegion', () => {
    test('takes the first preferred tag that names a region', () => {
        expect(systemRegion(['zh-Hant', 'zh-Hant-HK', 'en-GB'])).toBe('HK');
    });

    test('keeps a numeric region the browser reports', () => {
        expect(systemRegion(['es-419', 'es'])).toBe('419');
    });

    test('falls back to the likely region of the first tag', () => {
        expect(systemRegion(['en'])).toBe('US');
        expect(systemRegion(['ja'])).toBe('JP');
    });

    test('is undefined when the browser reports nothing', () => {
        expect(systemRegion([])).toBeUndefined();
    });

    test('skips a malformed tag rather than failing', () => {
        expect(systemRegion(['not a tag', 'fr-CA'])).toBe('CA');
    });
});

describe('formattingLocale', () => {
    test('substitutes the region where the runtime has data for the pair', () => {
        expect(formattingLocale('zh-Hant', 'HK')).toBe('zh-Hant-HK');
        expect(formattingLocale('en', 'DE')).toBe('en-DE');
        expect(formattingLocale('pt-BR', 'PT')).toBe('pt-PT');
        expect(formattingLocale('fr', 'CA')).toBe('fr-CA');
        expect(formattingLocale('es', '419')).toBe('es-419');
    });

    test("uses the region's own locale where the pair has no data", () => {
        expect(formattingLocale('zh-Hant', 'GB')).toBe('en-GB');
        expect(formattingLocale('fr', 'US')).toBe('en-US');
        expect(formattingLocale('de', 'US')).toBe('en-US');
        expect(formattingLocale('ja', 'DE')).toBe('de-DE');
        expect(formattingLocale('en', 'TW')).toBe('zh-TW');
    });

    test('uses the language alone without a usable region', () => {
        expect(formattingLocale('fr', undefined)).toBe('fr');
        expect(formattingLocale('fr', 'ZZ')).toBe('fr');
    });

    const date = new Date(Date.UTC(2026, 0, 31, 12));
    const utc: Intl.DateTimeFormatOptions = { timeZone: 'UTC' };
    const numericDate = (language: string, region: string) =>
        new Intl.DateTimeFormat(formattingLocale(language, region), utc).format(
            date,
        );
    const grouped = (language: string, region: string) =>
        new Intl.NumberFormat(formattingLocale(language, region)).format(
            1234567.5,
        );

    // The contract: region drives formatting, for every language.
    test('region decides date order', () => {
        expect(numericDate('en', 'US')).toBe('1/31/2026');
        expect(numericDate('en', 'GB')).toBe('31/01/2026');
        // CLDR has no zh-Hant-GB: without the fallback this is 2026/1/31.
        expect(numericDate('zh-Hant', 'GB')).toBe('31/01/2026');
        // CLDR has no fr-US: without the fallback this is 31/01/2026.
        expect(numericDate('fr', 'US')).toBe('1/31/2026');
    });

    test('region decides number grouping', () => {
        expect(grouped('en', 'US')).toBe('1,234,567.5');
        expect(grouped('en', 'DE')).toBe('1.234.567,5');
        // CLDR has no de-US or ja-DE: without the fallback these follow de and ja.
        expect(grouped('de', 'US')).toBe('1,234,567.5');
        expect(grouped('ja', 'DE')).toBe('1.234.567,5');
        // es-419 groups unlike es.
        expect(grouped('es', '419')).toBe('1,234,567.5');
    });

    test('language keeps month names where the pair has data', () => {
        const long = new Intl.DateTimeFormat(formattingLocale('fr', 'CA'), {
            ...utc,
            month: 'long',
        }).format(date);
        expect(long).toBe('janvier');
    });

    test('every supported language follows the region it is paired with', () => {
        const languages = [
            'en',
            'zh-Hans',
            'zh-Hant',
            'ja',
            'ko',
            'fr',
            'de',
            'es',
            'pt-BR',
            'it',
            'ru',
            'hi',
            'ar',
        ];
        for (const language of languages) {
            for (const region of ['US', 'GB', 'DE', 'JP', 'HK', '419']) {
                const resolved = new Intl.DateTimeFormat(
                    formattingLocale(language, region),
                ).resolvedOptions().locale;
                expect(new Intl.Locale(resolved).region).toBe(region);
            }
        }
    });
});

describe('region preference', () => {
    test('absent means follow the OS', () => {
        const storage = memoryStorage();
        expect(readRegionPreference(storage)).toBeUndefined();
        expect(resolveRegion(storage, ['en-GB'])).toBe('GB');
    });

    test('a stored override wins over the OS', () => {
        const storage = memoryStorage();
        writeRegionPreference(storage, 'jp');
        expect(storage.data.get(REGION_STORAGE_KEY)).toBe('JP');
        expect(resolveRegion(storage, ['en-GB'])).toBe('JP');
    });

    test('clearing the override follows the OS again', () => {
        const storage = memoryStorage({ [REGION_STORAGE_KEY]: 'JP' });
        writeRegionPreference(storage, undefined);
        expect(storage.data.has(REGION_STORAGE_KEY)).toBe(false);
        expect(resolveRegion(storage, ['en-GB'])).toBe('GB');
    });

    test('a corrupt stored value is ignored', () => {
        const storage = memoryStorage({ [REGION_STORAGE_KEY]: 'nowhere' });
        expect(readRegionPreference(storage)).toBeUndefined();
    });

    test('blocked storage follows the OS and does not throw', () => {
        expect(readRegionPreference(blockedStorage)).toBeUndefined();
        expect(() => writeRegionPreference(blockedStorage, 'GB')).not.toThrow();
        expect(resolveRegion(blockedStorage, ['en-CA'])).toBe('CA');
    });

    test('no storage at all follows the OS', () => {
        expect(resolveRegion(undefined, ['de-AT'])).toBe('AT');
    });
});

describe('regionOptions', () => {
    test('lists named, user-facing regions sorted in the display language', () => {
        const options = regionOptions('en');
        const codes = options.map((option) => option.code);
        expect(codes).toContain('GB');
        expect(codes).toContain('HK');
        expect(codes).not.toContain('ZZ');
        expect(codes).not.toContain('EU');
        // A numeric region with formatting data is offered; a continent is not.
        expect(codes).toContain('419');
        expect(codes).not.toContain('002');
        expect(options.find((option) => option.code === 'GB')?.name).toBe(
            'United Kingdom',
        );
        const names = options.map((option) => option.name);
        const collator = new Intl.Collator('en');
        expect([...names].sort(collator.compare)).toEqual(names);
    });

    test('names regions in the UI language, not their own', () => {
        const options = regionOptions('fr');
        expect(options.find((option) => option.code === 'DE')?.name).toBe(
            'Allemagne',
        );
    });
});
