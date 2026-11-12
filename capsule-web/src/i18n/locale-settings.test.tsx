// The locale provider hands Radix the language's direction, so a submenu opens and
// arrow keys move toward the inline end under an RTL language.

import { afterEach, describe, expect, test } from 'bun:test';
import { useDirection } from '@radix-ui/react-direction';
import { renderToString } from 'react-dom/server';

import { LANGUAGE_STORAGE_KEY } from './locale';
import { LocaleSettingsProvider } from './locale-settings';

function Probe() {
    return <span>{useDirection()}</span>;
}

const original = {
    localStorage: Object.getOwnPropertyDescriptor(globalThis, 'localStorage'),
    languages: Object.getOwnPropertyDescriptor(navigator, 'languages'),
};

function renderWithLanguage(language: string): string {
    const stored = new Map([[LANGUAGE_STORAGE_KEY, language]]);
    Object.defineProperty(globalThis, 'localStorage', {
        configurable: true,
        value: {
            getItem: (key: string) => stored.get(key) ?? null,
            setItem: (key: string, value: string) => stored.set(key, value),
            removeItem: (key: string) => stored.delete(key),
        },
    });
    Object.defineProperty(navigator, 'languages', {
        configurable: true,
        value: ['en'],
    });
    return renderToString(
        <LocaleSettingsProvider>
            <Probe />
        </LocaleSettingsProvider>,
    );
}

afterEach(() => {
    for (const [target, key, descriptor] of [
        [globalThis, 'localStorage', original.localStorage],
        [navigator, 'languages', original.languages],
    ] as const) {
        if (descriptor) {
            Object.defineProperty(target, key, descriptor);
        } else {
            delete (target as Record<string, unknown>)[key];
        }
    }
});

describe('LocaleSettingsProvider direction', () => {
    test('gives Radix rtl under Arabic', () => {
        expect(renderWithLanguage('ar')).toContain('>rtl<');
    });

    test('gives Radix ltr under English', () => {
        expect(renderWithLanguage('en')).toContain('>ltr<');
    });
});
