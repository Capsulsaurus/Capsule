// The docs site's locales and the path rules that follow from them. English is
// Starlight's root locale, served without a prefix; every other locale lives
// under `/<key>/`, with its translated pages in `src/content/docs/<key>/`.
// Only the user-facing pages are translated (TRANSLATED_PAGES); Starlight
// serves every other page at the locale's route in English, with its fallback
// notice.
//
// The set mirrors the product's locales (locales/config.json: tags and
// display names), which src/lib/locales.test.mjs holds it to. It is a literal
// rather than a read of that file so the header components, the Markdown
// pipeline and the docs-truth checks (Node built-ins only) share it without
// touching the filesystem.
//
// Pure data and functions: no Astro or Starlight imports.

/**
 * A docs locale. `key` is the route prefix and content directory (lowercase,
 * as Starlight expects), `lang` the BCP-47 tag, `label` the language's own
 * name, `dir` its writing direction.
 *
 * @typedef {object} DocsLocale
 * @property {string} key
 * @property {string} lang
 * @property {string} label
 * @property {'ltr' | 'rtl'} dir
 */

/** The source locale: Starlight's root, unprefixed. */
export const ROOT_LOCALE = Object.freeze({
    lang: 'en',
    label: 'English',
    dir: 'ltr',
});

/** @type {readonly DocsLocale[]} */
export const DOCS_LOCALES = Object.freeze(
    [
        { lang: 'zh-Hans', label: '简体中文' },
        { lang: 'zh-Hant', label: '繁體中文' },
        { lang: 'ja', label: '日本語' },
        { lang: 'ko', label: '한국어' },
        { lang: 'fr', label: 'Français' },
        { lang: 'de', label: 'Deutsch' },
        { lang: 'es', label: 'Español' },
        { lang: 'pt-BR', label: 'Português (Brasil)' },
        { lang: 'it', label: 'Italiano' },
        { lang: 'ru', label: 'Русский' },
        { lang: 'hi', label: 'हिन्दी' },
        { lang: 'ar', label: 'العربية', dir: 'rtl' },
    ].map(({ lang, label, dir = 'ltr' }) =>
        Object.freeze({ key: lang.toLowerCase(), lang, label, dir }),
    ),
);

/** The route prefixes of the non-root locales. */
export const LOCALE_KEYS = Object.freeze(DOCS_LOCALES.map(({ key }) => key));

const KEYS = new Set(LOCALE_KEYS);

/**
 * The pages every locale translates, as content paths relative to
 * `src/content/docs/`. Every other page falls back to English.
 */
export const TRANSLATED_PAGES = Object.freeze([
    'index.mdx',
    'support.mdx',
    'guides/getting-started.md',
    'guides/user-guide.md',
    'guides/self-hosting.md',
    'guides/troubleshooting.md',
]);

/**
 * First path segments the site serves once for every locale, outside
 * Starlight: the Pages Functions' routes (`functions/`), the static files in
 * `public/` (except `_headers`, which the host reads and never serves), and
 * the build's own asset and search roots. A link into one is never prefixed
 * with a locale. src/lib/locales.test.mjs holds the list to `functions/` and
 * `public/`.
 */
export const SHARED_ROOTS = Object.freeze([
    'rustdoc',
    'v',
    'images',
    'favicon.svg',
    'openapi.json',
    'versions.json',
    '_astro',
    'pagefind',
]);

const SHARED = new Set(SHARED_ROOTS);

/**
 * Starlight's `locales` option: English at the root, then every locale.
 *
 * @returns {Record<string, { label: string, lang: string, dir?: 'rtl' }>}
 */
export function starlightLocales() {
    return Object.fromEntries([
        ['root', { label: ROOT_LOCALE.label, lang: ROOT_LOCALE.lang }],
        ...DOCS_LOCALES.map(({ key, label, lang, dir }) => [
            key,
            dir === 'rtl' ? { label, lang, dir } : { label, lang },
        ]),
    ]);
}

/**
 * The locale a site path is under, and the path without it. A path under no
 * locale (English, or a shared root such as `/rustdoc/`) has no locale and is
 * returned as is. Accepts a route (`/ja/guides/`) or a content path
 * (`ja/guides/x.md`); the leading slash is kept as given.
 *
 * @param {string} path
 * @returns {{ locale: string | undefined, path: string }}
 */
export function splitLocale(path) {
    const match = /^(\/?)([^/?#]+)(?=$|[/?#])(.*)$/.exec(path);
    if (!match || !KEYS.has(match[2])) return { locale: undefined, path };
    const [, slash, locale, rest] = match;
    const tail = rest.replace(/^\//, '');
    return { locale, path: slash ? `/${tail}` : tail };
}

/**
 * A site-absolute path as `locale` serves it: under `/<locale>/`, or as is
 * for the root locale (`undefined`). External, protocol-relative, in-page and
 * relative links, paths already under a locale, and paths under a shared root
 * (SHARED_ROOTS) are left alone.
 *
 * @param {string} href
 * @param {string | undefined} locale A key of DOCS_LOCALES, or undefined.
 * @returns {string}
 */
export function localizePath(href, locale) {
    if (!locale || !href.startsWith('/') || href.startsWith('//')) return href;
    if (splitLocale(href).locale !== undefined) return href;
    const first = /^\/([^/?#]+)/.exec(href)?.[1];
    if (first !== undefined && SHARED.has(first)) return href;
    return `/${locale}${href}`;
}

/**
 * The BCP-47 tag of a locale key, `en` for the root locale.
 *
 * @param {string | undefined} locale
 * @returns {string}
 */
export function langOf(locale) {
    if (locale === undefined || locale === 'root') return ROOT_LOCALE.lang;
    const found = DOCS_LOCALES.find(({ key }) => key === locale);
    if (!found) throw new Error(`unknown docs locale: ${locale}`);
    return found.lang;
}
