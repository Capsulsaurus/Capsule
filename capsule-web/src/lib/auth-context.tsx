/**
 * Auth context providing current user state, login/logout actions,
 * and automatic token refresh via TanStack Query.
 */

import { useQuery, useQueryClient } from '@tanstack/react-query';
import type React from 'react';
import { createContext, useCallback, useContext, useEffect } from 'react';
import {
    type UserProfile,
    logout as apiLogout,
    refreshAccessToken as apiRefresh,
    getProfile,
} from './api';
import {
    type TokenPair,
    clearTokens,
    getTokenExpiry,
    hasTokens,
    saveTokens,
} from './auth';

export interface AuthState {
    /** The currently authenticated user, or null if not logged in. */
    user: UserProfile | null;
    /** True while the auth state is being determined (initial load). */
    isLoading: boolean;
    /** True if the user is authenticated. */
    isAuthenticated: boolean;
    /** Store new tokens after successful login/register. */
    setTokens: (tokens: TokenPair) => void;
    /** Log out and clear all tokens. */
    logout: () => Promise<void>;
}

const AuthContext = createContext<AuthState | null>(null);

export function useAuth(): AuthState {
    const ctx = useContext(AuthContext);
    if (!ctx) throw new Error('useAuth must be used within AuthProvider');
    return ctx;
}

export function AuthProvider({ children }: { children: React.ReactNode }) {
    const queryClient = useQueryClient();

    // Fetch the current user profile. Only runs when we have tokens.
    const {
        data: user,
        isLoading,
        refetch,
    } = useQuery({
        queryKey: ['auth', 'profile'],
        queryFn: getProfile,
        enabled: hasTokens(),
        retry: false,
        staleTime: 5 * 60 * 1000, // 5 min
    });

    // Proactively refresh the access token before expiry.
    useEffect(() => {
        if (!hasTokens()) return;

        const scheduleRefresh = () => {
            const expiry = getTokenExpiry();
            if (!expiry) return;
            const nowSecs = Date.now() / 1000;
            // Refresh 60 seconds before expiry
            const delayMs = Math.max((expiry - nowSecs - 60) * 1000, 0);
            return setTimeout(async () => {
                const refreshed = await apiRefresh();
                if (refreshed) {
                    scheduleRefresh();
                } else {
                    clearTokens();
                    queryClient.setQueryData(['auth', 'profile'], null);
                }
            }, delayMs);
        };

        const timer = scheduleRefresh();
        return () => {
            if (timer) clearTimeout(timer);
        };
    }, [queryClient]);

    const setTokens = useCallback(
        (tokens: TokenPair) => {
            saveTokens(tokens);
            refetch();
        },
        [refetch],
    );

    const handleLogout = useCallback(async () => {
        try {
            await apiLogout();
        } catch {
            clearTokens();
        }
        queryClient.setQueryData(['auth', 'profile'], null);
        queryClient.clear();
    }, [queryClient]);

    const isAuthenticated = !!user;

    return (
        <AuthContext.Provider
            value={{
                user: user ?? null,
                isLoading,
                isAuthenticated,
                setTokens,
                logout: handleLogout,
            }}
        >
            {children}
        </AuthContext.Provider>
    );
}
