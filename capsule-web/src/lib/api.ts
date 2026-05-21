/**
 * Typed API client for the Capsule auth REST API.
 * Automatically injects Authorization headers and refreshes tokens.
 */

import {
    type TokenPair,
    clearTokens,
    getAccessToken,
    getRefreshToken,
    isAccessTokenValid,
    saveTokens,
} from './auth';

const API_BASE = import.meta.env.PUBLIC_API_URL ?? 'http://localhost:3000';
const AUTH_BASE = `${API_BASE}/v1/auth`;

export class ApiError extends Error {
    constructor(
        public readonly status: number,
        message: string,
    ) {
        super(message);
        this.name = 'ApiError';
    }
}

async function parseError(res: Response): Promise<ApiError> {
    try {
        const body = await res.json();
        return new ApiError(
            res.status,
            body.error ?? body.message ?? res.statusText,
        );
    } catch {
        return new ApiError(res.status, res.statusText);
    }
}

/** Attempt to refresh the access token using the stored refresh token. */
export async function refreshAccessToken(): Promise<boolean> {
    const refreshToken = getRefreshToken();
    if (!refreshToken) return false;

    try {
        const res = await fetch(`${AUTH_BASE}/refresh`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ refresh_token: refreshToken }),
        });
        if (!res.ok) {
            clearTokens();
            return false;
        }
        const tokens: TokenPair = await res.json();
        saveTokens(tokens);
        return true;
    } catch {
        return false;
    }
}

/**
 * Authenticated fetch wrapper. Injects Bearer token, auto-refreshes
 * if needed, and redirects to /login on 401.
 */
export async function authFetch(
    path: string,
    init: RequestInit = {},
): Promise<Response> {
    // Ensure we have a valid access token
    if (!isAccessTokenValid()) {
        const refreshed = await refreshAccessToken();
        if (!refreshed) {
            clearTokens();
            window.location.href = '/login';
            throw new ApiError(401, 'Session expired');
        }
    }

    const token = getAccessToken();
    if (!token) throw new ApiError(401, 'Session expired');
    const headers = new Headers(init.headers);
    headers.set('Authorization', `Bearer ${token}`);
    headers.set(
        'Content-Type',
        headers.get('Content-Type') ?? 'application/json',
    );

    const res = await fetch(`${AUTH_BASE}${path}`, { ...init, headers });

    if (res.status === 401) {
        clearTokens();
        window.location.href = '/login';
        throw new ApiError(401, 'Unauthorized');
    }

    return res;
}

// ── Auth endpoints ──────────────────────────────────────────────────────────

export interface LoginRequest {
    email: string;
    password: string;
}

export interface LoginMfaRequiredResponse {
    mfa_required: true;
    mfa_token: string;
}

export async function login(
    body: LoginRequest,
): Promise<TokenPair | LoginMfaRequiredResponse> {
    const res = await fetch(`${AUTH_BASE}/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export interface RegisterRequest {
    username: string;
    name: string;
    email: string;
    password: string;
}

export async function register(body: RegisterRequest): Promise<TokenPair> {
    const res = await fetch(`${AUTH_BASE}/register`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export async function logout(): Promise<void> {
    try {
        await authFetch('/logout', { method: 'POST' });
    } finally {
        clearTokens();
    }
}

// ── TOTP endpoints ──────────────────────────────────────────────────────────

export async function verifyTotpLogin(
    mfaToken: string,
    totpCode: string,
): Promise<TokenPair> {
    const res = await fetch(`${AUTH_BASE}/login/verify-totp`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mfa_token: mfaToken, totp_code: totpCode }),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export interface TotpEnrollResponse {
    provisioning_uri: string;
}

export async function totpEnroll(): Promise<TotpEnrollResponse> {
    const res = await authFetch('/totp/enroll', { method: 'POST' });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export async function totpVerifyEnrollment(totpCode: string): Promise<void> {
    const res = await authFetch('/totp/verify-enrollment', {
        method: 'POST',
        body: JSON.stringify({ totp_code: totpCode }),
    });
    if (!res.ok) throw await parseError(res);
}

export async function totpDisable(totpCode: string): Promise<void> {
    const res = await authFetch('/totp/disable', {
        method: 'POST',
        body: JSON.stringify({ totp_code: totpCode }),
    });
    if (!res.ok) throw await parseError(res);
}

// ── Passkey endpoints ───────────────────────────────────────────────────────

export async function passkeyLoginStart(username?: string): Promise<unknown> {
    const res = await fetch(`${AUTH_BASE}/passkey/login/start`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username }),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export async function passkeyLoginFinish(
    credential: unknown,
): Promise<TokenPair> {
    const res = await fetch(`${AUTH_BASE}/passkey/login/finish`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(credential),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export async function passkeyRegisterStart(): Promise<unknown> {
    const res = await authFetch('/passkey/register/start', { method: 'POST' });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export async function passkeyRegisterFinish(
    credential: unknown,
    name?: string,
): Promise<void> {
    const body = { ...(credential as object), name };
    const res = await authFetch('/passkey/register/finish', {
        method: 'POST',
        body: JSON.stringify(body),
    });
    if (!res.ok) throw await parseError(res);
}

export interface PasskeyCredential {
    id: string;
    name: string;
    created_at: number;
}

export async function listPasskeys(): Promise<PasskeyCredential[]> {
    const res = await authFetch('/passkey/credentials');
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export async function deletePasskey(credId: string): Promise<void> {
    const res = await authFetch(`/passkey/credentials/${credId}`, {
        method: 'DELETE',
    });
    if (!res.ok) throw await parseError(res);
}

// ── Profile endpoints ───────────────────────────────────────────────────────

export interface UserProfile {
    id: string;
    username: string;
    name: string;
    email: string;
    profile_image_url?: string;
    needs_onboarding: boolean;
    is_admin: boolean;
}

export async function getProfile(): Promise<UserProfile> {
    const res = await authFetch('/profile');
    if (!res.ok) throw await parseError(res);
    return res.json();
}

export interface UpdateProfileRequest {
    username?: string;
    email?: string;
    current_password?: string;
    new_password?: string;
}

export async function updateProfile(
    body: UpdateProfileRequest,
): Promise<UserProfile> {
    const res = await authFetch('/profile', {
        method: 'POST',
        body: JSON.stringify(body),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

// ── Devices endpoints ───────────────────────────────────────────────────────

export interface Device {
    id: string;
    created_at: number;
    last_active_at: number;
    user_agent?: string;
    ip_address?: string;
    is_current: boolean;
}

export async function getDevices(): Promise<Device[]> {
    const res = await authFetch('/devices');
    if (!res.ok) throw await parseError(res);
    return res.json();
}

// ── Password reset ──────────────────────────────────────────────────────────

export async function requestPasswordReset(email: string): Promise<void> {
    const res = await fetch(`${AUTH_BASE}/password-reset-request`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ email }),
    });
    if (!res.ok) throw await parseError(res);
}

export async function resetPassword(
    token: string,
    newPassword: string,
): Promise<void> {
    const res = await fetch(`${AUTH_BASE}/password-reset`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token, new_password: newPassword }),
    });
    if (!res.ok) throw await parseError(res);
}
