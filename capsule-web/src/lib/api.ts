/**
 * Typed client for the Capsule auth REST API (slice `S-C62`).
 *
 * Every function here corresponds to an operation in `capsule-server/openapi.json`, and the file
 * is hand-written rather than generated because the browser holds no Rust and `capsule-sdk`'s
 * generated client is not reachable from it. That makes it the one place the web app can drift
 * from the served contract silently — which is exactly what it did when the Salvo tree retired,
 * so the shapes below are pinned against the document and the drift is what `api.test.ts` looks
 * for.
 */

import {
    clearTokens,
    getAccessToken,
    getRefreshToken,
    isAccessTokenValid,
    saveTokens,
    type TokenPair,
} from './auth';

const API_BASE = import.meta.env.PUBLIC_API_URL ?? 'http://localhost:3000';
const AUTH_BASE = `${API_BASE}/v1/auth`;

export class ApiError extends Error {
    constructor(
        public readonly status: number,
        message: string,
        /** The stable `error.*` code, when the server sent an RFC 9457 problem. */
        public readonly code?: string,
    ) {
        super(message);
        this.name = 'ApiError';
    }
}

async function parseError(res: Response): Promise<ApiError> {
    try {
        const body = await res.json();
        // RFC 9457: `detail` is the human-readable half and `code` is the stable catalog key a
        // client switches on. `error`/`message` were the Salvo envelope's and are kept as a
        // fallback only so a proxy's own body does not read as an empty message.
        return new ApiError(
            res.status,
            body.detail ?? body.error ?? body.message ?? res.statusText,
            typeof body.code === 'string' ? body.code : undefined,
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

/**
 * The half-finished sign-in the server answers with `202` when the account has a confirmed
 * second factor (`S-C55`).
 *
 * `mfaRequired` is set **here**, from the status, and is not a field the server sends: the
 * server distinguishes the two outcomes by status because that is what a status is for, and
 * inventing a discriminator in the body would be a second place for the two to disagree.
 */
export interface LoginMfaRequiredResponse {
    mfaRequired: true;
    mfa_token: string;
    expires_by: number;
}

/** Whether a login answer still needs a code. */
export function needsSecondFactor(
    result: TokenPair | LoginMfaRequiredResponse,
): result is LoginMfaRequiredResponse {
    return 'mfaRequired' in result;
}

export async function login(
    body: LoginRequest,
): Promise<TokenPair | LoginMfaRequiredResponse> {
    const res = await fetch(`${AUTH_BASE}/login`, {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            Accept: 'application/json',
        },
        body: JSON.stringify(body),
    });
    if (!res.ok) throw await parseError(res);
    // 202 Accepted: the credentials were accepted and the request is not complete.
    if (res.status === 202) {
        const challenge = await res.json();
        return { ...challenge, mfaRequired: true as const };
    }
    return res.json();
}

/**
 * An address and a password, and nothing else (`S-C53`).
 *
 * The `username` and `name` this used to carry are gone: each was a fact the server stored about
 * a person, and a display name belongs to the profile surface. Sending them now is a `422` — the
 * body is strict.
 */
export interface RegisterRequest {
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

/**
 * Complete a sign-in with a code.
 *
 * This is the request that opens the session, which is why the advisory `cohort_hash` would ride
 * here rather than on `login` — the browser sends neither today, so a web session shows in the
 * devices view without a cohort, which is honest.
 */
export async function verifyTotpLogin(
    mfaToken: string,
    totpCode: string,
): Promise<TokenPair> {
    const res = await fetch(`${AUTH_BASE}/login/verify-totp`, {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            Accept: 'application/json',
        },
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

// ── Profile endpoints ───────────────────────────────────────────────────────

/**
 * The whole of what the server stores about a person (`S-C54`).
 *
 * `username`, `name`, `profile_image_url`, `needs_onboarding` and `is_admin` are gone — the
 * server never had the last two, and the first three were fields it stored about a person for no
 * one's benefit.
 */
export interface UserProfile {
    user_id: string;
    email: string;
    display_name?: string;
    created_at: string;
}

export async function getProfile(): Promise<UserProfile> {
    const res = await authFetch('/profile');
    if (!res.ok) throw await parseError(res);
    return res.json();
}

/**
 * A partial edit. An absent `display_name` leaves it alone; an explicit `null` clears it.
 *
 * There is no `email` field, and its absence is a decision rather than an oversight: changing a
 * login address needs proof the caller controls the new one, and this deployment has no way to
 * obtain it. See the Profile Surface in the authentication design doc.
 */
export interface UpdateProfileRequest {
    display_name?: string | null;
}

export async function updateProfile(
    body: UpdateProfileRequest,
): Promise<UserProfile> {
    const res = await authFetch('/profile', {
        method: 'PATCH',
        body: JSON.stringify(body),
    });
    if (!res.ok) throw await parseError(res);
    return res.json();
}

/**
 * Replace the password, authenticated by the one it replaces.
 *
 * Every **other** session of the account ends; this one is re-opened under its own id, so the
 * caller's tokens keep working and nobody else's do. Answers `204`.
 */
export async function changePassword(
    currentPassword: string,
    newPassword: string,
): Promise<void> {
    const res = await authFetch('/password', {
        method: 'POST',
        body: JSON.stringify({
            current_password: currentPassword,
            new_password: newPassword,
        }),
    });
    if (!res.ok) throw await parseError(res);
}

// ── Devices endpoints ───────────────────────────────────────────────────────

/** One live session of the account. */
export interface Device {
    session_id: string;
    created_at: string;
    authenticated_at: string;
    last_active_at: string;
    user_agent?: string;
    ip_address?: string;
    cohort_hash?: string;
    device_id?: string;
    current: boolean;
}

/** One physical device, grouping its re-enrollments (`S-C13`). */
export interface Cohort {
    cohort_hash: string;
    first_seen: string;
    last_seen: string;
}

export interface DevicesResponse {
    sessions: Device[];
    cohorts: Cohort[];
}

export async function getDevices(): Promise<DevicesResponse> {
    const res = await authFetch('/devices');
    if (!res.ok) throw await parseError(res);
    return res.json();
}

/** End one session of this account by id. */
export async function revokeDevice(sessionId: string): Promise<void> {
    const res = await authFetch(`/devices/${sessionId}`, { method: 'DELETE' });
    if (!res.ok) throw await parseError(res);
}
// ── Recovery ────────────────────────────────────────────────────────────────
//
// There is no password reset, and there will not be one (`S-C54`). On an end-to-end-encrypted
// account a server-issued reset is not a recovery: the server cannot re-wrap a master key it has
// never seen, so a "reset" would return an account whose library is unreadable. The recovery
// story is the escrow blob at `GET`/`PUT /v1/auth/escrow`, unwrapped client-side with the user's
// own recovery secret — see the backup-and-recovery design doc. What the surface *does* offer is
// a password **change**, above, authenticated by the password it replaces.
