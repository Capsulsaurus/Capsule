import { describe, expect, test } from 'bun:test';

import {
    applyDocumentLocale,
    directionFor,
    LANGUAGE_STORAGE_KEY,
    languageOptions,
    messagesFor,
    normalizeLanguage,
    RTL_LOCALES,
    readLanguagePreference,
    resolveLanguage,
    resolveLocale,
    SUPPORTED_LOCALES,
    writeLanguagePreference,
} from './locale';

describe('official locale set', () => {
    test('ships English plus the twelve official translations', () => {
        expect([...SUPPORTED_LOCALES]).toEqual([
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
        ]);
    });

    test('every supported locale has a non-empty catalog', () => {
        for (const locale of SUPPORTED_LOCALES) {
            expect(Object.keys(messagesFor(locale)).length).toBeGreaterThan(0);
        }
    });
});

describe('RTL wiring', () => {
    test('Arabic is the only right-to-left locale', () => {
        expect([...RTL_LOCALES]).toEqual(['ar']);
    });

    test('directionFor is rtl for Arabic and ltr for the rest', () => {
        expect(directionFor('ar')).toBe('rtl');
        for (const locale of SUPPORTED_LOCALES) {
            if (locale === 'ar') {
                continue;
            }
            expect(directionFor(locale)).toBe('ltr');
        }
    });

    // The smoke: drive the exact code path index.tsx runs at startup and assert
    // the app root ends up mirrored (dir="rtl") under Arabic.
    test('applyDocumentLocale mirrors the document under Arabic', () => {
        const root = { lang: '', dir: '' };
        applyDocumentLocale('ar', root);
        expect(root.dir).toBe('rtl');
        expect(root.lang).toBe('ar');
    });

    test('applyDocumentLocale leaves an LTR locale unmirrored', () => {
        const root = { lang: '', dir: '' };
        applyDocumentLocale('en', root);
        expect(root.dir).toBe('ltr');
        expect(root.lang).toBe('en');
    });

    test('an Arabic browser resolves to the Arabic (RTL) locale', () => {
        expect(resolveLocale(['ar-EG', 'en'])).toBe('ar');
        expect(directionFor(resolveLocale(['ar-EG', 'en']))).toBe('rtl');
    });
});

function memoryStorage(initial: Record<string, string> = {}) {
    const data = new Map(Object.entries(initial));
    return {
        data,
        getItem: (key: string) => data.get(key) ?? null,
        setItem: (key: string, value: string) => {
            data.set(key, value);
        },
        removeItem: (key: string) => {
            data.delete(key);
        },
    };
}

describe('language preference', () => {
    test('absent means negotiate from the OS', () => {
        expect(resolveLanguage(memoryStorage(), ['ja-JP', 'en'])).toBe('ja');
    });

    test('a stored override wins over the OS', () => {
        const storage = memoryStorage();
        writeLanguagePreference(storage, 'ZH-HANT');
        expect(storage.data.get(LANGUAGE_STORAGE_KEY)).toBe('zh-Hant');
        expect(readLanguagePreference(storage)).toBe('zh-Hant');
        expect(resolveLanguage(storage, ['en-GB'])).toBe('zh-Hant');
    });

    test('clearing the override follows the OS again', () => {
        const storage = memoryStorage({ [LANGUAGE_STORAGE_KEY]: 'fr' });
        writeLanguagePreference(storage, undefined);
        expect(storage.data.has(LANGUAGE_STORAGE_KEY)).toBe(false);
        expect(resolveLanguage(storage, ['de-CH'])).toBe('de');
    });

    test('an unsupported stored language is ignored', () => {
        const storage = memoryStorage({ [LANGUAGE_STORAGE_KEY]: 'tlh' });
        expect(readLanguagePreference(storage)).toBeUndefined();
        expect(resolveLanguage(storage, ['ko'])).toBe('ko');
    });

    test('blocked storage negotiates from the OS and does not throw', () => {
        const blocked = {
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
        expect(() => writeLanguagePreference(blocked, 'fr')).not.toThrow();
        expect(resolveLanguage(blocked, ['it'])).toBe('it');
    });

    test('normalizeLanguage accepts only supported tags', () => {
        expect(normalizeLanguage('pt-br')).toBe('pt-BR');
        expect(normalizeLanguage('pt')).toBeUndefined();
        expect(normalizeLanguage(null)).toBeUndefined();
    });
});

describe('languageOptions', () => {
    test('offers every supported language, labelled in itself', () => {
        const options = languageOptions();
        expect(options.map((option) => option.code)).toEqual([
            ...SUPPORTED_LOCALES,
        ]);
        expect(options.find((option) => option.code === 'fr')?.name).toBe(
            'français',
        );
        expect(options.find((option) => option.code === 'ja')?.name).toBe(
            '日本語',
        );
    });
});
