// Render tests under the development pseudo-locales (i18n design doc, Layout → Pseudo-locales).
//
// - Expanded (`en-XA`): every visible text node on the main screens is pseudo-localized. A
//   catalogued `en-XA` message is bracketed, so a node without the brackets never went
//   through the catalog — a hardcoded string `i18n-guard`'s pattern scan missed.
// - Right to left (`ar-XB`): the direction reaches the document root and every Radix
//   primitive, the screens take their text from the right-to-left catalog, and no rendered
//   element is laid out with a physical (left/right) class that would not mirror.
//
// The screens render to a string, as the rest of this suite does: there is no DOM here, so
// what is asserted is the markup and the classes the browser would lay out.

import { afterEach, describe, expect, test } from 'bun:test';
import { useDirection } from '@radix-ui/react-direction';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
    createMemoryHistory,
    createRootRoute,
    createRouter,
    RouterProvider,
} from '@tanstack/react-router';
import type { ComponentType } from 'react';
import { renderToString } from 'react-dom/server';

import { AppSidebar } from '@/components/app-sidebar';
import { Header } from '@/components/header';
import { TotpSignInCard } from '@/components/mfa/totp-sign-in';
import { AuthProvider } from '@/lib/auth-context';
import { APP_NAME } from '@/lib/constant';
import { Route as AlbumsRoute } from '@/routes/albums/index';
import { Route as ExploreRoute } from '@/routes/explore.lazy';
import { Route as LoginRoute } from '@/routes/login.lazy';
import { Route as PhotosRoute } from '@/routes/photos.lazy';
import { Route as LanguageRoute } from '@/routes/settings/language.lazy';
import { Route as SettingsRoute } from '@/routes/settings.lazy';
import { Route as SharingRoute } from '@/routes/sharing.lazy';
import {
    applyDocumentLocale,
    LANGUAGE_STORAGE_KEY,
    resolveLanguage,
} from './locale';
import { LocaleSettingsProvider } from './locale-settings';

/**
 * Text that is the same in every language and so is deliberately not catalogued: the
 * product name. Anything else that renders unmarked is a hardcoded string.
 */
const VERBATIM = new Set([APP_NAME]);

/** Right-to-left mark: every `ar-XB` message opens with one. */
const RLM = '‏';

/** The main screens, each rendered inside the app shell (header and sidebar). */
const SCREENS: [path: string, screen: ComponentType][] = [
    ['/photos', component(PhotosRoute)],
    ['/explore', component(ExploreRoute)],
    ['/sharing', component(SharingRoute)],
    ['/albums', component(AlbumsRoute)],
    ['/settings', component(SettingsRoute)],
    ['/settings/language', component(LanguageRoute)],
    ['/login', component(LoginRoute)],
    ['/login', TotpStep],
];

function component(route: { options: { component?: unknown } }): ComponentType {
    const screen = route.options.component;
    if (typeof screen !== 'function') {
        throw new Error('route has no component');
    }
    return screen as ComponentType;
}

/** The sign-in's second step, which carries the screen's Back control. */
function TotpStep() {
    return (
        <TotpSignInCard mfaToken="" onSignedIn={() => {}} onBack={() => {}} />
    );
}

function DirectionProbe() {
    return <output data-direction={useDirection()} />;
}

const original = {
    localStorage: Object.getOwnPropertyDescriptor(globalThis, 'localStorage'),
    languages: Object.getOwnPropertyDescriptor(navigator, 'languages'),
};

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

/** Choose `language` the way a person does: stored in the language preference. */
function chooseLanguage(language: string) {
    const stored = new Map([[LANGUAGE_STORAGE_KEY, language]]);
    const storage = {
        getItem: (key: string) => stored.get(key) ?? null,
        setItem: (key: string, value: string) => {
            stored.set(key, value);
        },
        removeItem: (key: string) => {
            stored.delete(key);
        },
    };
    Object.defineProperty(globalThis, 'localStorage', {
        configurable: true,
        value: storage,
    });
    Object.defineProperty(navigator, 'languages', {
        configurable: true,
        value: ['en'],
    });
    return storage;
}

/** Render `screen` at `path` inside the app shell, under `language`. */
async function render(
    language: string,
    path: string,
    Screen: ComponentType,
): Promise<string> {
    chooseLanguage(language);
    const root = createRootRoute({
        component: () => (
            <>
                <Header />
                <AppSidebar />
                <main>
                    <Screen />
                </main>
                <DirectionProbe />
            </>
        ),
    });
    const router = createRouter({
        routeTree: root,
        history: createMemoryHistory({ initialEntries: [path] }),
    });
    await router.load();
    return renderToString(
        <LocaleSettingsProvider>
            <QueryClientProvider client={new QueryClient()}>
                <AuthProvider>
                    <RouterProvider router={router} />
                </AuthProvider>
            </QueryClientProvider>
        </LocaleSettingsProvider>,
    );
}

const ENTITIES: Record<string, string> = {
    '&amp;': '&',
    '&lt;': '<',
    '&gt;': '>',
    '&quot;': '"',
    '&#x27;': "'",
    '&#39;': "'",
    '&nbsp;': ' ',
};

function decode(text: string): string {
    return text.replace(/&(?:amp|lt|gt|quot|nbsp|#x27|#39);/g, (e) => {
        return ENTITIES[e] ?? e;
    });
}

/**
 * Removes every HTML comment, repeating until none is left, so a comment that one pass
 * splices back together (`<!-<!---->-->`) is removed too.
 */
function stripComments(html: string): string {
    let previous: string;
    let current = html;
    do {
        previous = current;
        current = current.replace(/<!--[\s\S]*?-->/g, '');
    } while (current !== previous);
    return current;
}

/**
 * Every piece of text a person sees or hears: text nodes, plus the attributes a browser
 * shows (`placeholder`, `title`, `alt`) or announces (`aria-label`).
 *
 * A picker option whose value is a language or region code is left out: its label is
 * the CLDR display name `Intl.DisplayNames` produces, which is data rather than an app
 * string and is never in the catalog. The picker's own catalogued "system default"
 * option has the empty value and is kept.
 */
function visibleText(html: string): string[] {
    const withoutComments = stripComments(html).replace(
        /<option value="[^"]+"[^>]*>[^<]*<\/option>/g,
        '',
    );
    const nodes = withoutComments
        .split(/<[^>]*>/)
        .map((node) => decode(node).trim())
        .filter((node) => node.length > 0);
    const attributes = [
        ...withoutComments.matchAll(
            /\s(?:placeholder|title|alt|aria-label)="([^"]*)"/g,
        ),
    ]
        .map((match) => decode(match[1]).trim())
        .filter((value) => value.length > 0);
    return [...nodes, ...attributes];
}

/** Every class token rendered, for the physical-direction check. */
function classTokens(html: string): string[] {
    return [...html.matchAll(/\sclass="([^"]*)"/g)].flatMap((match) =>
        match[1].split(/\s+/),
    );
}

/**
 * A Tailwind utility that names a physical side, so it lays out the same in both
 * directions. Its logical counterpart (`ms-`, `pe-`, `start-`, `text-end`, …) mirrors.
 * Variant prefixes (`rtl:`, `md:`) are stripped first; an `rtl:` override is how a
 * deliberately physical transform mirrors, so it is allowed.
 */
function isPhysical(token: string): boolean {
    if (token.startsWith('rtl:') || token.startsWith('ltr:')) {
        return false;
    }
    const utility = token.slice(token.lastIndexOf(':') + 1).replace(/^-/, '');
    return /^(?:m[lr]|p[lr]|left|right|border-[lr]|rounded-[lr]|rounded-[tb][lr]|text-(?:left|right)|float-(?:left|right)|clear-(?:left|right)|scroll-[mp][lr])(?:-|$)/.test(
        utility,
    );
}

describe('expanded pseudo-locale (en-XA)', () => {
    for (const [path, Screen] of SCREENS) {
        test(`every visible string on ${path} (${Screen.name}) is catalogued`, async () => {
            const html = await render('en-XA', path, Screen);
            const text = visibleText(html);
            expect(text.length).toBeGreaterThan(0);
            expect(text.filter(isUnmarkedEnXa)).toEqual([]);
        });
    }

    test('a hardcoded string on a screen is caught', async () => {
        function Hardcoded() {
            return <p>Upload complete</p>;
        }
        const html = await render('en-XA', '/photos', Hardcoded);
        expect(visibleText(html).filter(isUnmarkedEnXa)).toEqual([
            'Upload complete',
        ]);
    });
});

/** Whether `node` is visible text that did not come through the `en-XA` catalog. */
function isUnmarkedEnXa(node: string): boolean {
    return !VERBATIM.has(node) && !(node.startsWith('[') && node.endsWith(']'));
}

describe('right-to-left pseudo-locale (ar-XB)', () => {
    test('the startup path mirrors the document root', () => {
        const storage = chooseLanguage('ar-XB');
        const root = { lang: '', dir: '' };
        applyDocumentLocale(resolveLanguage(storage, ['en']), root);
        expect(root).toEqual({ lang: 'ar-XB', dir: 'rtl' });
    });

    for (const [path, Screen] of SCREENS) {
        test(`${path} (${Screen.name}) lays out right to left`, async () => {
            const html = await render('ar-XB', path, Screen);
            expect(html).toContain('data-direction="rtl"');
            const unmarked = visibleText(html).filter(
                (node) => !VERBATIM.has(node) && !node.startsWith(RLM),
            );
            expect(unmarked).toEqual([]);
            expect(classTokens(html).filter(isPhysical)).toEqual([]);
        });
    }

    test('the Back control takes the right-to-left catalog', async () => {
        const html = await render('ar-XB', '/login', TotpStep);
        expect(html).toContain(`>${RLM}‮Back‬${RLM}</button>`);
    });

    test('the physical-class check recognises what it must refuse', () => {
        for (const token of [
            'ml-2',
            'pr-4',
            '-left-1',
            'md:right-0',
            'text-left',
            'border-l',
            'rounded-tl-md',
        ]) {
            expect(isPhysical(token)).toBe(true);
        }
        for (const token of [
            'ms-2',
            'pe-4',
            'start-0',
            'text-start',
            'border-e',
            'rtl:-translate-x-5',
            'rtl:rotate-180',
            'translate-x-5',
            'leading-none',
        ]) {
            expect(isPhysical(token)).toBe(false);
        }
    });
});
