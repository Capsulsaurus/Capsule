// The language and region settings, held for the whole app.
//
// Both default from the OS and are device preferences (never synced to the
// server), so they live in `localStorage`. The provider renders the
// `IntlProvider`: messages come from the language's catalog, while the
// provider's `locale` is the formatting locale, so every `intl.formatDate` and
// `intl.formatNumber` follows the region.

import {
    createContext,
    type ReactNode,
    useCallback,
    useContext,
    useLayoutEffect,
    useMemo,
    useState,
} from 'react';
import { IntlProvider } from 'react-intl';

import {
    applyDocumentLocale,
    messagesFor,
    readLanguagePreference,
    resolveLocale,
    SOURCE_LOCALE,
    writeLanguagePreference,
} from './locale';
import {
    formattingLocale,
    type PreferenceStorage,
    readRegionPreference,
    systemRegion,
    writeRegionPreference,
} from './region';

export interface LocaleSettings {
    /** The UI language in effect (a supported catalog tag). */
    language: string;
    /** The formatting region in effect, or `undefined` when none is known. */
    region: string | undefined;
    /** The stored language override; `undefined` follows the OS. */
    languagePreference: string | undefined;
    /** The stored region override; `undefined` follows the OS. */
    regionPreference: string | undefined;
    /** What the OS alone would pick for the language. */
    systemLanguage: string;
    /** What the OS alone would pick for the region. */
    systemRegion: string | undefined;
    setLanguagePreference: (language: string | undefined) => void;
    setRegionPreference: (region: string | undefined) => void;
}

const LocaleSettingsContext = createContext<LocaleSettings | null>(null);

function browserStorage(): PreferenceStorage | undefined {
    try {
        return globalThis.localStorage;
    } catch {
        return undefined;
    }
}

export function LocaleSettingsProvider({ children }: { children: ReactNode }) {
    const storage = browserStorage();
    const [languagePreference, setLanguageState] = useState(() =>
        readLanguagePreference(storage),
    );
    const [regionPreference, setRegionState] = useState(() =>
        readRegionPreference(storage),
    );

    const preferred = navigator.languages;
    const systemLanguage = resolveLocale(preferred);
    const osRegion = systemRegion(preferred);
    const language = languagePreference ?? systemLanguage;
    const region = regionPreference ?? osRegion;

    // Reflect the language (never the region) onto <html lang>/<html dir> so the
    // app mirrors under an RTL language. A layout effect, so the first paint is
    // already mirrored.
    useLayoutEffect(() => {
        applyDocumentLocale(language);
    }, [language]);

    const setLanguagePreference = useCallback(
        (next: string | undefined) => {
            writeLanguagePreference(storage, next);
            setLanguageState(readLanguagePreference(storage) ?? next);
        },
        [storage],
    );
    const setRegionPreference = useCallback(
        (next: string | undefined) => {
            writeRegionPreference(storage, next);
            setRegionState(readRegionPreference(storage) ?? next);
        },
        [storage],
    );

    const value = useMemo<LocaleSettings>(
        () => ({
            language,
            region,
            languagePreference,
            regionPreference,
            systemLanguage,
            systemRegion: osRegion,
            setLanguagePreference,
            setRegionPreference,
        }),
        [
            language,
            region,
            languagePreference,
            regionPreference,
            systemLanguage,
            osRegion,
            setLanguagePreference,
            setRegionPreference,
        ],
    );

    return (
        <LocaleSettingsContext.Provider value={value}>
            <IntlProvider
                locale={formattingLocale(language, region)}
                defaultLocale={SOURCE_LOCALE}
                messages={messagesFor(language)}
            >
                {children}
            </IntlProvider>
        </LocaleSettingsContext.Provider>
    );
}

export function useLocaleSettings(): LocaleSettings {
    const settings = useContext(LocaleSettingsContext);
    if (!settings) {
        throw new Error(
            'useLocaleSettings must be used inside LocaleSettingsProvider',
        );
    }
    return settings;
}
