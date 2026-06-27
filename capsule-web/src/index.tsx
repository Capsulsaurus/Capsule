import { RouterProvider, createRouter } from '@tanstack/react-router';
import { StrictMode } from 'react';
import ReactDOM from 'react-dom/client';
import { IntlProvider } from 'react-intl';

import { SOURCE_LOCALE, messagesFor, resolveLocale } from '@/i18n/locale';

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
