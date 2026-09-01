import { describe, expect, test } from 'bun:test';

import {
    applyDocumentLocale,
    directionFor,
    messagesFor,
    RTL_LOCALES,
    resolveLocale,
    SUPPORTED_LOCALES,
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
