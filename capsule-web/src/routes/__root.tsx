import {
    Outlet,
    createRootRoute,
    useNavigate,
    useRouterState,
} from '@tanstack/react-router';

import { AppSidebar } from '@/components/app-sidebar';
import { Header } from '@/components/header';
import { AuthProvider, useAuth } from '@/lib/auth-context';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import React, { Suspense, useEffect } from 'react';

const queryClient = new QueryClient();

const TanStackRouterDevtools =
    process.env.NODE_ENV === 'production'
        ? () => null // Render nothing in production
        : React.lazy(() =>
              import('@tanstack/router-devtools').then((res) => ({
                  default: res.TanStackRouterDevtools,
              })),
          );

const ReactQueryDevtoolsProduction = React.lazy(() =>
    import('@tanstack/react-query-devtools/build/modern/production.js').then(
        (d) => ({
            default: d.ReactQueryDevtools,
        }),
    ),
);

/** Paths that do not require authentication */
const PUBLIC_PATHS = [
    '/login',
    '/register',
    '/forgot-password',
    '/reset-password',
];

function AuthGuard({ children }: { children: React.ReactNode }) {
    const { isLoading, isAuthenticated } = useAuth();
    const navigate = useNavigate();
    const { location } = useRouterState();
    const pathname = location.pathname;

    const isPublic = PUBLIC_PATHS.some(
        (p) => pathname === p || pathname.startsWith(`${p}/`),
    );

    useEffect(() => {
        if (!isLoading && !isAuthenticated && !isPublic) {
            navigate({ to: '/login', replace: true });
        }
    }, [isLoading, isAuthenticated, isPublic, navigate]);

    // Show nothing while checking auth on protected routes (avoid content flash)
    if (isLoading && !isPublic) {
        return null;
    }

    // Don't render protected content until authenticated
    if (!isAuthenticated && !isPublic) {
        return null;
    }

    return <>{children}</>;
}

export const Route = createRootRoute({
    component: () => {
        const [showDevtools, setShowDevtools] = React.useState(false);

        React.useEffect(() => {
            // @ts-expect-error
            window.toggleDevtools = () => setShowDevtools((old) => !old);
        }, []);

        return (
            <QueryClientProvider client={queryClient}>
                <AuthProvider>
                    <AuthGuard>
                        <div className="flex flex-col h-screen">
                            <Header />
                            <div className="flex flex-1 overflow-hidden">
                                <AppSidebar className="hidden md:flex flex-shrink-0" />
                                <main className="flex-1 overflow-y-auto bg-background">
                                    <Outlet />
                                </main>
                            </div>
                        </div>
                    </AuthGuard>
                    <Suspense>
                        <TanStackRouterDevtools />
                    </Suspense>

                    <ReactQueryDevtools initialIsOpen={false} />
                    {showDevtools && (
                        <React.Suspense fallback={null}>
                            <ReactQueryDevtoolsProduction />
                        </React.Suspense>
                    )}
                </AuthProvider>
            </QueryClientProvider>
        );
    },
});
