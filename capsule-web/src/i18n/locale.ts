// Locale resolution for the web client.
//
// Messages are compiled from the canonical `locales/` catalogs by `mise run i18n`
// (see the i18n design doc); they are not authored here. To add a locale, add it
// to `locales/config.json`, translate it, regenerate, and import its bundle below.

import arMessages from './messages/ar.json';
import arXbMessages from './messages/ar-XB.json';
import deMessages from './messages/de.json';
import enMessages from './messages/en.json';
import enXaMessages from './messages/en-XA.json';
import esMessages from './messages/es.json';
import frMessages from './messages/fr.json';
import hiMessages from './messages/hi.json';
import itMessages from './messages/it.json';
import jaMessages from './messages/ja.json';
import koMessages from './messages/ko.json';
import ptBrMessages from './messages/pt-BR.json';
import ruMessages from './messages/ru.json';
import zhHansMessages from './messages/zh-Hans.json';
import zhHantMessages from './messages/zh-Hant.json';
import type { PreferenceStorage } from './region';

/** The source (authoring) locale — the final fallback. Mirrors `locales/config.json`. */
export const SOURCE_LOCALE = 'en';

/** Supported locales. Mirrors `locales/config.json`; extend when adding a locale. */
export const SUPPORTED_LOCALES = [
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
] as const;

/**
 * Locales whose script is written right-to-left. Adding Arabic put RTL layout in
 * scope for every client; the web surface expresses it via the document `dir`
 * attribute (native clients mirror their own layout). Mirrors the RTL set in the
 * i18n design doc's Supported Languages section.
 */
export const RTL_LOCALES = ['ar'] as const;

type LayoutDirection = 'ltr' | 'rtl';

type Messages = Record<string, string>;

const CATALOGS: Record<string, Messages> = {
    en: enMessages,
    'zh-Hans': zhHansMessages,
    'zh-Hant': zhHantMessages,
    ja: jaMessages,
    ko: koMessages,
    fr: frMessages,
    de: deMessages,
    es: esMessages,
    'pt-BR': ptBrMessages,
    it: itMessages,
    ru: ruMessages,
    hi: hiMessages,
    ar: arMessages,
};

/**
 * The development pseudo-locales, generated from the source catalog by `mise run i18n`:
 * `en-XA` (accented, about 40% longer, bracketed) and `ar-XB` (right to left). They are
 * never in `SUPPORTED_LOCALES`; a development build offers them in the language picker
 * and a production build compiles them out.
 */
export const PSEUDO_LOCALES = ['en-XA', 'ar-XB'] as const;

/** The right-to-left pseudo-locale, laid out as Arabic is. */
const PSEUDO_RTL_LOCALES: readonly string[] = ['ar-XB'];

/**
 * Whether this is a development build. Rsbuild replaces `process.env.NODE_ENV` at build
 * time, so in production the pseudo bundles below are unreachable and tree-shaken.
 */
export const isDevelopmentBuild = (): boolean =>
    process.env.NODE_ENV !== 'production';

const PSEUDO_CATALOGS: Record<string, Messages> =
    process.env.NODE_ENV === 'production'
        ? {}
        : { 'en-XA': enXaMessages, 'ar-XB': arXbMessages };

/** The languages this build can show: the supported set, plus the pseudo-locales in development. */
export function availableLocales(
    development: boolean = isDevelopmentBuild(),
): readonly string[] {
    return development
        ? [...SUPPORTED_LOCALES, ...PSEUDO_LOCALES]
        : SUPPORTED_LOCALES;
}

/** Pick the best supported locale for the browser, falling back to the source. */
export function resolveLocale(
    preferred: readonly string[] = navigator.languages,
): string {
    for (const tag of preferred) {
        const lower = tag.toLowerCase();
        const exact = SUPPORTED_LOCALES.find(
            (locale) => locale.toLowerCase() === lower,
        );
        if (exact) {
            return exact;
        }
        const primary = lower.split('-')[0];
        const byPrimary = SUPPORTED_LOCALES.find(
            (locale) => locale.split('-')[0].toLowerCase() === primary,
        );
        if (byPrimary) {
            return byPrimary;
        }
    }
    return SOURCE_LOCALE;
}

/** The flat ICU message catalog for `locale` (falls back to the source locale). */
export function messagesFor(locale: string): Messages {
    return (
        CATALOGS[locale] ?? PSEUDO_CATALOGS[locale] ?? CATALOGS[SOURCE_LOCALE]
    );
}

/** The writing direction for `locale` — `'rtl'` for Arabic and `ar-XB`, `'ltr'` otherwise. */
export function directionFor(locale: string): LayoutDirection {
    return (RTL_LOCALES as readonly string[]).includes(locale) ||
        PSEUDO_RTL_LOCALES.includes(locale)
        ? 'rtl'
        : 'ltr';
}

/**
 * Reflect `locale` onto the document root so the whole app mirrors under an RTL
 * locale: sets `<html lang>` and `<html dir>`. This is the single place the web
 * client turns a locale into layout direction. Accepts the target element so it
 * can be unit-tested without a live DOM.
 */
export function applyDocumentLocale(
    locale: string,
    element: Pick<HTMLElement, 'lang' | 'dir'> = document.documentElement,
): void {
    element.lang = locale;
    element.dir = directionFor(locale);
}

/** A stored preference: the key in `Storage`, absent meaning "follow the OS". */
export const LANGUAGE_STORAGE_KEY = 'capsule.language';

/**
 * The available locale `tag` names exactly (case-insensitively), else `undefined`. A
 * pseudo-locale is available only in a development build, so a production build ignores
 * one left in storage and follows the OS instead.
 */
export function normalizeLanguage(
    tag: string | null | undefined,
    development: boolean = isDevelopmentBuild(),
): string | undefined {
    if (!tag) {
        return undefined;
    }
    const lower = tag.trim().toLowerCase();
    return availableLocales(development).find(
        (locale) => locale.toLowerCase() === lower,
    );
}

/** The stored language override, or `undefined` when the user follows the OS. */
export function readLanguagePreference(
    storage: PreferenceStorage | undefined,
): string | undefined {
    try {
        return normalizeLanguage(storage?.getItem(LANGUAGE_STORAGE_KEY));
    } catch {
        // Storage can throw when the browser blocks it; treat as "follow the OS".
        return undefined;
    }
}

/** Store a language override, or clear it (`undefined`) to follow the OS again. */
export function writeLanguagePreference(
    storage: PreferenceStorage | undefined,
    language: string | undefined,
): void {
    const normalized = normalizeLanguage(language);
    try {
        if (normalized) {
            storage?.setItem(LANGUAGE_STORAGE_KEY, normalized);
        } else {
            storage?.removeItem(LANGUAGE_STORAGE_KEY);
        }
    } catch {
        // A blocked store keeps the choice for this page load only.
    }
}

/** The effective UI language: the stored override, else negotiated from the OS. */
export function resolveLanguage(
    storage: PreferenceStorage | undefined,
    preferred: readonly string[] = navigator.languages,
): string {
    return readLanguagePreference(storage) ?? resolveLocale(preferred);
}

/** A pickable language, labelled in its own language so a reader can find theirs. */
export interface LanguageOption {
    code: string;
    name: string;
}

/**
 * Every supported language with its autonym, in `config.json` order, followed in a
 * development build by the pseudo-locales.
 */
export function languageOptions(
    development: boolean = isDevelopmentBuild(),
): LanguageOption[] {
    return availableLocales(development).map((code) => {
        let name: string | undefined;
        try {
            name = new Intl.DisplayNames([code], { type: 'language' }).of(code);
        } catch {
            name = undefined;
        }
        return { code, name: name ?? code };
    });
}
