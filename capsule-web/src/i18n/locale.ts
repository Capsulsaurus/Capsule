// Locale resolution for the web client.
//
// Messages are compiled from the canonical `locales/` catalogs by `just i18n`
// (see the i18n design doc); they are not authored here. To add a locale, add it
// to `locales/config.json`, translate it, regenerate, and import its bundle below.

import enMessages from './messages/en.json';

/** The source (authoring) locale — the final fallback. Mirrors `locales/config.json`. */
export const SOURCE_LOCALE = 'en';

/** Supported locales. Mirrors `locales/config.json`; extend when adding a locale. */
export const SUPPORTED_LOCALES = ['en'] as const;

type Messages = Record<string, string>;

const CATALOGS: Record<string, Messages> = {
    en: enMessages,
};

/** Pick the best supported locale for the browser, falling back to the source. */
export function resolveLocale(
    preferred: readonly string[] = navigator.languages,
): string {
    for (const tag of preferred) {
        const lower = tag.toLowerCase();
        const exact = SUPPORTED_LOCALES.find((locale) => locale === lower);
        if (exact) {
            return exact;
        }
        const primary = lower.split('-')[0];
        const byPrimary = SUPPORTED_LOCALES.find(
            (locale) => locale.split('-')[0] === primary,
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
