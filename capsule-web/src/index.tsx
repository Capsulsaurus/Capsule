import { RouterProvider, createRouter } from '@tanstack/react-router';
import { StrictMode } from 'react';
import ReactDOM from 'react-dom/client';
import { Client, Provider as UrqlProvider, fetchExchange } from 'urql';

// import schema from './schema';

// Import the generated route tree
import { routeTree } from './routeTree.gen';

import './index.css';
import { ThemeProvider } from '@/components/theme-provider';
import { Toaster } from '@/components/ui/sonner';

// const exchanges = [];

// if (import.meta.env.DEV) {
//     await import('@urql/devtools').then(({ devtoolsExchange }) => {
//         exchanges.push(devtoolsExchange);
//     });
// }

// const storage = makeDefaultStorage({
//     idbName: 'graphcache-v3', // The name of the IndexedDB database
//     maxAge: 7, // The maximum age of the persisted data in days
// });

// exchanges.push(
//     // @populate retrieves data to merge into the cache
//     populateExchange({
//         schema,
//     }),
//     // provides offline support
//     offlineExchange({
//         schema,
//         storage,
//         // updates: {},
//         // optimistic: {},
//     }),
//     // enables persisted queries
//     persistedExchange({
//         preferGetForPersistedQueries: true,
//     }),
//     fetchExchange,
// );

const client = new Client({
    url: 'http://localhost:3000/graphql',
    exchanges: [fetchExchange],
}); // TODO: Add headers for auth

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
    const root = ReactDOM.createRoot(rootElement);
    root.render(
        <StrictMode>
            <UrqlProvider value={client}>
                <ThemeProvider>
                    <RouterProvider router={router} />
                    <Toaster />
                </ThemeProvider>
            </UrqlProvider>
        </StrictMode>,
    );
}

if ('serviceWorker' in navigator) {
    window.addEventListener('load', () => {
        navigator.serviceWorker
            .register('/service-worker.js')
            .then((registration) => {
                console.log(
                    'Service Worker registered with scope:',
                    registration.scope,
                );
            })
            .catch((error) => {
                console.error('Service Worker registration failed:', error);
            });
    });
}
