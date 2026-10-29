/**
 * The browser half of signing in through an identity provider.
 *
 * The server runs the PKCE exchange with the provider itself; what the browser adds is its own
 * binding to the ceremony (`binding_challenge` / `binding_verifier`), which is what stops a
 * `state` and `code` carried from an attacker's ceremony into this tab from signing the person in
 * to the attacker's library. The binding secret is generated here, its S256 hash goes to the
 * authorize, and the secret itself waits in `sessionStorage` — scoped to this tab, gone when the
 * tab closes — until the provider redirects back to {@link OIDC_CALLBACK_PATH}.
 *
 * Nothing here writes library state: the ceremony ends in the same token pair a password sign-in
 * opens, so the read-only web contract is untouched.
 */

/**
 * Where the provider sends the person back. The deployment registers
 * `<web origin>/login/oidc-callback` as its `OIDC_REDIRECT_URL` (and at the provider): the
 * server admits only that exact address, so the two must agree byte for byte.
 */
export const OIDC_CALLBACK_PATH = '/login/oidc-callback';

/** The `sessionStorage` key the pending ceremony is held under. */
export const CEREMONY_KEY = 'capsule_oidc_ceremony';

/** The 256-bit binding secret, as raw bytes. */
const VERIFIER_BYTES = 32;

/** One begun ceremony, as this tab remembers it until the redirect comes back. */
export interface PendingCeremony {
    /** The `state` the authorize answered with. */
    state: string;
    /** The binding secret whose S256 hash the authorize carried. */
    verifier: string;
    /** Absolute Unix seconds after which the server will not redeem the `state`. */
    expiresBy: number;
}

/** Unpadded base64url (RFC 4648 §5), the encoding the server's S256 check reads. */
export function base64Url(bytes: Uint8Array): string {
    let binary = '';
    for (const byte of bytes) binary += String.fromCharCode(byte);
    return btoa(binary)
        .replace(/\+/g, '-')
        .replace(/\//g, '_')
        .replace(/=+$/, '');
}

/** A fresh 256-bit binding secret: 43 base64url characters. */
export function freshBindingVerifier(): string {
    const bytes = new Uint8Array(VERIFIER_BYTES);
    crypto.getRandomValues(bytes);
    return base64Url(bytes);
}

/** The S256 hash of a binding secret: unpadded base64url of its SHA-256, 43 characters. */
export async function bindingChallenge(verifier: string): Promise<string> {
    const digest = await crypto.subtle.digest(
        'SHA-256',
        new TextEncoder().encode(verifier),
    );
    return base64Url(new Uint8Array(digest));
}

/** This client's callback address on the origin it is served from. */
export function callbackUri(origin: string): string {
    return `${origin}${OIDC_CALLBACK_PATH}`;
}

/** Remember a begun ceremony. One per tab: a second sign-in replaces the first. */
export function saveCeremony(
    storage: Storage,
    ceremony: PendingCeremony,
): void {
    storage.setItem(CEREMONY_KEY, JSON.stringify(ceremony));
}

/**
 * Take the binding secret for `state`, single-use: the stored ceremony is removed whatever the
 * outcome, so a reloaded or replayed callback finds nothing.
 *
 * `null` when this tab began no ceremony, began a different one, or began it so long ago the
 * server would refuse it anyway.
 */
export function takeCeremony(
    storage: Storage,
    state: string,
    nowSecs: number,
): string | null {
    const raw = storage.getItem(CEREMONY_KEY);
    storage.removeItem(CEREMONY_KEY);
    if (!raw) return null;
    let ceremony: Partial<PendingCeremony>;
    try {
        ceremony = JSON.parse(raw);
    } catch {
        return null;
    }
    if (
        typeof ceremony.state !== 'string' ||
        typeof ceremony.verifier !== 'string' ||
        typeof ceremony.expiresBy !== 'number'
    ) {
        return null;
    }
    if (ceremony.state !== state || nowSecs >= ceremony.expiresBy) return null;
    return ceremony.verifier;
}

/** What the provider's redirect carried. */
export type CallbackParams =
    | { kind: 'code'; state: string; code: string }
    /** The provider ended the ceremony itself (RFC 6749 §4.1.2.1), e.g. `access_denied`. */
    | { kind: 'provider_error'; error: string }
    /** Neither: a redirect nobody could finish. */
    | { kind: 'malformed' };

/** Read the redirect's query. A provider error wins over a code that rides beside it. */
export function readCallback(search: string): CallbackParams {
    const params = new URLSearchParams(search);
    const error = params.get('error');
    if (error) return { kind: 'provider_error', error };
    const state = params.get('state');
    const code = params.get('code');
    if (state && code) return { kind: 'code', state, code };
    return { kind: 'malformed' };
}
