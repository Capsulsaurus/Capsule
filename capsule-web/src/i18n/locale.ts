// Locale resolution for the web client.
//
// Messages are compiled from the canonical `locales/` catalogs by `mise run i18n`
// (see the i18n design doc); they are not authored here. To add a locale, add it
// to `locales/config.json`, translate it, regenerate, and import its bundle below.

import arMessages from './messages/ar.json';
import deMessages from './messages/de.json';
import enMessages from './messages/en.json';
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
    return CATALOGS[locale] ?? CATALOGS[SOURCE_LOCALE];
}

/** The writing direction for `locale` — `'rtl'` for Arabic, `'ltr'` otherwise. */
export function directionFor(locale: string): LayoutDirection {
    return (RTL_LOCALES as readonly string[]).includes(locale) ? 'rtl' : 'ltr';
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
