// The browser's binding to an identity-provider ceremony (issue #1000).
//
// The server refuses a `binding_challenge` that is not 43 unpadded base64url characters and a
// callback whose `binding_verifier` does not hash to it, so the encoding is pinned against RFC
// 7636's own S256 vector, and the single-use `sessionStorage` hand-off between the login and the
// callback is driven through every way it can be stale.

import { describe, expect, test } from 'bun:test';

import {
    base64Url,
    bindingChallenge,
    CEREMONY_KEY,
    callbackUri,
    freshBindingVerifier,
    OIDC_CALLBACK_PATH,
    readCallback,
    saveCeremony,
    takeCeremony,
} from './oidc';

/** A minimal `Storage`, for a runtime without one. */
class MemoryStorage implements Storage {
    private readonly items = new Map<string, string>();
    get length(): number {
        return this.items.size;
    }
    key(index: number): string | null {
        return [...this.items.keys()][index] ?? null;
    }
    getItem(key: string): string | null {
        return this.items.get(key) ?? null;
    }
    setItem(key: string, value: string): void {
        this.items.set(key, value);
    }
    removeItem(key: string): void {
        this.items.delete(key);
    }
    clear(): void {
        this.items.clear();
    }
}

const BASE64URL_43 = /^[A-Za-z0-9_-]{43}$/;

describe('the binding', () => {
    test('is RFC 7636 S256', async () => {
        // RFC 7636 Appendix B.
        expect(
            await bindingChallenge(
                'dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk',
            ),
        ).toBe('E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM');
    });

    test('verifier is a fresh 256-bit secret in 43 base64url characters', () => {
        const a = freshBindingVerifier();
        const b = freshBindingVerifier();
        expect(a).toMatch(BASE64URL_43);
        expect(b).toMatch(BASE64URL_43);
        expect(a).not.toBe(b);
    });

    test('challenge is the 43-character shape the server admits', async () => {
        expect(await bindingChallenge(freshBindingVerifier())).toMatch(
            BASE64URL_43,
        );
    });

    test('base64url is unpadded and URL-safe', () => {
        expect(base64Url(new Uint8Array([0xfb, 0xff]))).toBe('-_8');
        expect(base64Url(new Uint8Array([]))).toBe('');
    });
});

describe('the callback address', () => {
    test('is the deployment origin and the callback route', () => {
        expect(callbackUri('https://photos.example')).toBe(
            'https://photos.example/login/oidc-callback',
        );
        expect(OIDC_CALLBACK_PATH).toBe('/login/oidc-callback');
    });
});

describe('the pending ceremony', () => {
    const ceremony = { state: 'state-1', verifier: 'secret' };

    test('hands its secret to the callback for its own state', () => {
        const storage = new MemoryStorage();
        saveCeremony(storage, ceremony);
        expect(takeCeremony(storage, 'state-1')).toBe('secret');
    });

    test('is single-use', () => {
        const storage = new MemoryStorage();
        saveCeremony(storage, ceremony);
        takeCeremony(storage, 'state-1');
        expect(storage.getItem(CEREMONY_KEY)).toBeNull();
        expect(takeCeremony(storage, 'state-1')).toBeNull();
    });

    test('refuses another state, and is spent by it', () => {
        const storage = new MemoryStorage();
        saveCeremony(storage, ceremony);
        expect(takeCeremony(storage, 'state-2')).toBeNull();
        expect(storage.getItem(CEREMONY_KEY)).toBeNull();
    });

    test('leaves expiry to the server, whatever the browser clock says', () => {
        // A record carrying a long-past expiry still redeems: a fast browser clock must not
        // refuse a ceremony the server would accept, and a stale one is the server's to refuse.
        const storage = new MemoryStorage();
        storage.setItem(
            CEREMONY_KEY,
            JSON.stringify({ ...ceremony, expiresBy: 0 }),
        );
        expect(takeCeremony(storage, 'state-1')).toBe('secret');
    });

    test('is absent in a tab that began none', () => {
        expect(takeCeremony(new MemoryStorage(), 'state-1')).toBeNull();
    });

    test('ignores a corrupt or foreign record', () => {
        const storage = new MemoryStorage();
        storage.setItem(CEREMONY_KEY, '{not json');
        expect(takeCeremony(storage, 'state-1')).toBeNull();
        storage.setItem(CEREMONY_KEY, JSON.stringify({ state: 'state-1' }));
        expect(takeCeremony(storage, 'state-1')).toBeNull();
        expect(storage.getItem(CEREMONY_KEY)).toBeNull();
    });

    test('a second sign-in replaces the first', () => {
        const storage = new MemoryStorage();
        saveCeremony(storage, ceremony);
        saveCeremony(storage, { ...ceremony, state: 'state-2' });
        expect(takeCeremony(storage, 'state-2')).toBe('secret');
    });
});

describe('the redirect', () => {
    test('carries a state and a code', () => {
        expect(readCallback('?state=s&code=c')).toEqual({
            kind: 'code',
            state: 's',
            code: 'c',
        });
    });

    test('carries a provider error, which wins over a code', () => {
        expect(readCallback('?error=access_denied&state=s')).toEqual({
            kind: 'provider_error',
            error: 'access_denied',
        });
        expect(readCallback('?error=server_error&state=s&code=c')).toEqual({
            kind: 'provider_error',
            error: 'server_error',
        });
    });

    test('is malformed without both halves', () => {
        expect(readCallback('')).toEqual({ kind: 'malformed' });
        expect(readCallback('?state=s')).toEqual({ kind: 'malformed' });
        expect(readCallback('?code=c')).toEqual({ kind: 'malformed' });
    });
});
