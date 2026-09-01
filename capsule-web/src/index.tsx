import { createRouter, RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import ReactDOM from 'react-dom/client';
import { IntlProvider } from 'react-intl';

import {
    applyDocumentLocale,
    messagesFor,
    resolveLocale,
    SOURCE_LOCALE,
} from '@/i18n/locale';

// Import the generated route tree
import { routeTree } from './routeTree.gen';

import './index.css';
import { ThemeProvider } from '@/components/theme-provider';
import { Toaster } from '@/components/ui/sonner';

// Create a new router instance
const router = createRouter({ routeTree });

// Register the router instance for type safety
declare module '@tanstack/react-router' {
    interface Register {
        router: typeof router;
    }
}

// Render the app
const rootElement = document.getElementById('root');
if (rootElement) {
    const locale = resolveLocale();
    // Mirror the whole app under an RTL locale by reflecting the resolved locale
    // onto <html lang>/<html dir> before the first render.
    applyDocumentLocale(locale);
    const root = ReactDOM.createRoot(rootElement);
    root.render(
        <StrictMode>
            <IntlProvider
                locale={locale}
                defaultLocale={SOURCE_LOCALE}
                messages={messagesFor(locale)}
            >
                <ThemeProvider>
                    <RouterProvider router={router} />
                    <Toaster />
                </ThemeProvider>
            </IntlProvider>
        </StrictMode>,
    );
}
