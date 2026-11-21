import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
    DOCS_LOCALES,
    LOCALE_KEYS,
    langOf,
    localizePath,
    ROOT_LOCALE,
    SHARED_ROOTS,
    splitLocale,
    starlightLocales,
    TRANSLATED_PAGES,
} from './locales.mjs';

const at = (rel) => fileURLToPath(new URL(rel, import.meta.url));
const config = JSON.parse(
    readFileSync(at('../../../locales/config.json'), 'utf8'),
);

describe('DOCS_LOCALES', () => {
    it('mirrors the product locales in locales/config.json, English at the root', () => {
        expect(ROOT_LOCALE.lang).toBe(config.sourceLocale);
        expect(ROOT_LOCALE.label).toBe(config.displayNames.en);
        expect(DOCS_LOCALES.map(({ lang }) => lang)).toEqual(
            config.supportedLocales.filter(
                (lang) => lang !== config.sourceLocale,
            ),
        );
        for (const { lang, label } of DOCS_LOCALES) {
            expect(label).toBe(config.displayNames[lang]);
        }
    });

    it('keys each locale by its lowercased tag', () => {
        expect(LOCALE_KEYS).toEqual([
            'zh-hans',
            'zh-hant',
            'ja',
            'ko',
            'fr',
            'de',
            'es',
            'pt-br',
            'it',
            'ru',
            'hi',
            'ar',
        ]);
    });

    it('writes Arabic right to left and the rest left to right', () => {
        expect(
            DOCS_LOCALES.filter(({ dir }) => dir === 'rtl').map(
                ({ key }) => key,
            ),
        ).toEqual(['ar']);
    });
});

describe('starlightLocales', () => {
    it('puts English at the root and every locale under its key', () => {
        const locales = starlightLocales();
        expect(locales.root).toEqual({ label: 'English', lang: 'en' });
        expect(locales['pt-br']).toEqual({
            label: 'Português (Brasil)',
            lang: 'pt-BR',
        });
        expect(locales.ar).toEqual({
            label: 'العربية',
            lang: 'ar',
            dir: 'rtl',
        });
        expect(Object.keys(locales)).toEqual(['root', ...LOCALE_KEYS]);
    });
});

describe('splitLocale', () => {
    it.each([
        ['/ja/guides/x/', 'ja', '/guides/x/'],
        ['/zh-hans/', 'zh-hans', '/'],
        ['/zh-hans', 'zh-hans', '/'],
        ['/ar/#top', 'ar', '/#top'],
        ['pt-br/guides/x.md', 'pt-br', 'guides/x.md'],
        ['/guides/x/', undefined, '/guides/x/'],
        ['/javascript/', undefined, '/javascript/'],
        ['/ja-jp/', undefined, '/ja-jp/'],
        ['/rustdoc/ja/', undefined, '/rustdoc/ja/'],
        ['', undefined, ''],
    ])('%s -> %s %s', (path, locale, rest) => {
        expect(splitLocale(path)).toEqual({ locale, path: rest });
    });
});

describe('localizePath', () => {
    it.each([
        ['/guides/x/', 'ja', '/ja/guides/x/'],
        ['/', 'ar', '/ar/'],
        ['/design/#keys', 'de', '/de/design/#keys'],
        ['/guides/x/', undefined, '/guides/x/'],
        ['/ko/guides/x/', 'ja', '/ko/guides/x/'],
        ['https://example.com/', 'ja', 'https://example.com/'],
        ['//cdn.example.com/x', 'ja', '//cdn.example.com/x'],
        ['#anchor', 'ja', '#anchor'],
        ['../x/', 'ja', '../x/'],
        ['/rustdoc/capsule_sdk/', 'ja', '/rustdoc/capsule_sdk/'],
        ['/rustdoc', 'ar', '/rustdoc'],
        ['/v/0.4/guides/', 'de', '/v/0.4/guides/'],
        ['/images/hero-light.webp', 'ko', '/images/hero-light.webp'],
        ['/versions.json', 'fr', '/versions.json'],
        ['/rustdocs/', 'ja', '/ja/rustdocs/'],
        ['/images-old/x', 'ja', '/ja/images-old/x'],
    ])('%s in %s -> %s', (href, locale, expected) => {
        expect(localizePath(href, locale)).toBe(expected);
    });
});

describe('SHARED_ROOTS', () => {
    it('covers every Pages Function route and every served public/ entry', () => {
        const served = [
            ...readdirSync(at('../../functions'), { withFileTypes: true })
                .filter((entry) => entry.isDirectory())
                .map(({ name }) => name),
            ...readdirSync(at('../../public')).filter(
                (name) => name !== '_headers',
            ),
        ];
        for (const root of served) expect(SHARED_ROOTS, root).toContain(root);
    });

    it('shares no name with a locale', () => {
        for (const key of LOCALE_KEYS) expect(SHARED_ROOTS).not.toContain(key);
    });
});

describe('langOf', () => {
    it('maps a key to its tag, the root to English', () => {
        expect(langOf('zh-hant')).toBe('zh-Hant');
        expect(langOf(undefined)).toBe('en');
        expect(langOf('root')).toBe('en');
        expect(() => langOf('xx')).toThrow(/unknown docs locale/);
    });
});

describe('TRANSLATED_PAGES', () => {
    it('names English pages that exist', () => {
        for (const page of TRANSLATED_PAGES) {
            expect(existsSync(at(`../content/docs/${page}`)), page).toBe(true);
        }
    });

    it.each(
        LOCALE_KEYS,
    )('is exactly what src/content/docs/%s/ holds', (key) => {
        const pages = readdirSync(at(`../content/docs/${key}`), {
            recursive: true,
            withFileTypes: true,
        })
            .filter((entry) => entry.isFile())
            .map((entry) =>
                relative(
                    at(`../content/docs/${key}`),
                    join(entry.parentPath, entry.name),
                )
                    .split(sep)
                    .join('/'),
            );
        expect(pages.sort()).toEqual([...TRANSLATED_PAGES].sort());
    });
});

describe('script-tagged Chinese UI strings', () => {
    // Starlight looks its built-in dictionaries up by language and region
    // (zh-CN, zh-TW), so src/content/i18n/ carries them for zh-Hans and
    // zh-Hant. They must keep every key Starlight's own carries.
    it.each([
        ['zh-Hans', 'zh-CN'],
        ['zh-Hant', 'zh-TW'],
    ])('%s carries every key of Starlight %s', async (lang, builtin) => {
        const ours = JSON.parse(
            readFileSync(at(`../content/i18n/${lang}.json`), 'utf8'),
        );
        const theirs = (
            await import(
                at(
                    `../../node_modules/@astrojs/starlight/dist/translations/${builtin}.js`,
                )
            )
        ).default;
        expect(Object.keys(ours).sort()).toEqual(Object.keys(theirs).sort());
    });
});
