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

    test('refuses what is not a user region', () => {
        for (const code of [
            undefined,
            null,
            '',
            'G',
            'GBR',
            '419',
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
    test('substitutes the region into the UI language', () => {
        expect(formattingLocale('zh-Hant', 'GB')).toBe('zh-Hant-GB');
        expect(formattingLocale('en', 'DE')).toBe('en-DE');
        expect(formattingLocale('pt-BR', 'PT')).toBe('pt-PT');
    });

    test('uses the language alone without a usable region', () => {
        expect(formattingLocale('fr', undefined)).toBe('fr');
        expect(formattingLocale('fr', 'ZZ')).toBe('fr');
    });

    // The contract: region drives formatting and language keeps the words.
    test('region decides date order while language keeps month names', () => {
        const date = new Date(Date.UTC(2026, 0, 31, 12));
        const options: Intl.DateTimeFormatOptions = { timeZone: 'UTC' };
        expect(
            new Intl.DateTimeFormat(
                formattingLocale('en', 'US'),
                options,
            ).format(date),
        ).toBe('1/31/2026');
        expect(
            new Intl.DateTimeFormat(
                formattingLocale('en', 'GB'),
                options,
            ).format(date),
        ).toBe('31/01/2026');
        const long = new Intl.DateTimeFormat(formattingLocale('fr', 'US'), {
            ...options,
            month: 'long',
        }).format(date);
        expect(long).toBe('janvier');
    });

    test('region decides number grouping', () => {
        expect(
            new Intl.NumberFormat(formattingLocale('en', 'US')).format(1234.5),
        ).toBe('1,234.5');
        expect(
            new Intl.NumberFormat(formattingLocale('en', 'DE')).format(1234.5),
        ).toBe('1.234,5');
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
