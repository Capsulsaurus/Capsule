/**
 * Auth token storage and management utilities.
 * Tokens are stored in localStorage. The access token is refreshed
 * automatically before expiry via the auth context.
 */

const ACCESS_TOKEN_KEY = 'capsule_access_token';
const REFRESH_TOKEN_KEY = 'capsule_refresh_token';
const TOKEN_EXPIRY_KEY = 'capsule_token_expiry';

export interface TokenPair {
    access_token: string;
    refresh_token: string;
    token_type: string;
    expires_by: number; // unix seconds
}

export function saveTokens(tokens: TokenPair): void {
    localStorage.setItem(ACCESS_TOKEN_KEY, tokens.access_token);
    localStorage.setItem(REFRESH_TOKEN_KEY, tokens.refresh_token);
    localStorage.setItem(TOKEN_EXPIRY_KEY, String(tokens.expires_by));
}

export function clearTokens(): void {
    localStorage.removeItem(ACCESS_TOKEN_KEY);
    localStorage.removeItem(REFRESH_TOKEN_KEY);
    localStorage.removeItem(TOKEN_EXPIRY_KEY);
}

export function getAccessToken(): string | null {
    return localStorage.getItem(ACCESS_TOKEN_KEY);
}

export function getRefreshToken(): string | null {
    return localStorage.getItem(REFRESH_TOKEN_KEY);
}

export function getTokenExpiry(): number | null {
    const v = localStorage.getItem(TOKEN_EXPIRY_KEY);
    return v ? Number(v) : null;
}

/** Returns true if the access token is present and not expiring within the next 30 seconds. */
export function isAccessTokenValid(): boolean {
    const token = getAccessToken();
    if (!token) return false;
    const expiry = getTokenExpiry();
    if (!expiry) return false;
    const nowSecs = Date.now() / 1000;
    return expiry - nowSecs > 30;
}

/** Returns true if any auth tokens exist (user is potentially logged in). */
export function hasTokens(): boolean {
    return !!getAccessToken() && !!getRefreshToken();
}
