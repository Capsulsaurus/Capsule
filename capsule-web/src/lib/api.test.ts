// The browser client sends the protocol handshake (issue #404, decision 24).
//
// Every gated route refuses a request without `X-Capsule-Protocol`, and `api.ts` is hand-written
// — the browser holds no Rust — so this is the one place the header can silently go missing
// again. Driven against a recording mock of the global `fetch`: each of the five request
// builders is called once and the header it sent is compared with `PROTOCOL_VERSION`, and
// `PROTOCOL_VERSION` itself is compared with the literal in the Rust source of truth, read at
// test time, so the restated constant cannot drift from `capsule_core`.

import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import {
    authFetch,
    login,
    PROTOCOL_VERSION,
    refreshAccessToken,
    register,
    verifyTotpLogin,
} from './api';

/** One request the mock saw. */
interface Call {
    url: string;
    protocol: string | null;
}

/** A minimal `localStorage`, for a runtime without one. Only the four methods `auth.ts` uses. */
class MemoryStorage {
    private readonly items = new Map<string, string>();
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

const realFetch = globalThis.fetch;
const realStorage = (globalThis as { localStorage?: unknown }).localStorage;
const calls: Call[] = [];

/** Every request succeeds with a token pair, so no builder takes its failure branch. */
function recordingFetch(): typeof fetch {
    return (async (input: RequestInfo | URL, init?: RequestInit) => {
        const url =
            typeof input === 'string'
                ? input
                : input instanceof URL
                  ? input.toString()
                  : input.url;
        const headers = new Headers(
            init?.headers ??
                (input instanceof Request ? input.headers : undefined),
        );
        calls.push({ url, protocol: headers.get('X-Capsule-Protocol') });
        return new Response(
            JSON.stringify({
                access_token: 'access',
                refresh_token: 'refresh',
                token_type: 'Bearer',
                expires_by: Math.floor(Date.now() / 1000) + 3600,
            }),
            { status: 200, headers: { 'Content-Type': 'application/json' } },
        );
    }) as typeof fetch;
}

beforeEach(() => {
    calls.length = 0;
    globalThis.fetch = recordingFetch();
    (globalThis as { localStorage: unknown }).localStorage =
        new MemoryStorage();
    // A live session, so `authFetch` neither refreshes nor redirects.
    localStorage.setItem('capsule_access_token', 'access');
    localStorage.setItem('capsule_refresh_token', 'refresh');
    localStorage.setItem(
        'capsule_token_expiry',
        String(Math.floor(Date.now() / 1000) + 3600),
    );
});

afterEach(() => {
    globalThis.fetch = realFetch;
    (globalThis as { localStorage?: unknown }).localStorage = realStorage;
});

/** The one request the mock saw, and its handshake. */
function theRequest(): Call {
    expect(calls).toHaveLength(1);
    return calls[0];
}

describe('the protocol handshake', () => {
    test('is the version capsule-core speaks', () => {
        // `import.meta.dir` is `capsule-web/src/lib`; three levels up is the repository root.
        const primitives = readFileSync(
            join(
                import.meta.dir,
                '..',
                '..',
                '..',
                'capsule-core',
                'src',
                'crypto',
                'primitives.rs',
            ),
            'utf8',
        );
        const declared = primitives.match(
            /pub const PROTOCOL_VERSION: &str = "(\d{4}-\d{2}-\d{2})";/,
        );
        expect(declared).not.toBeNull();
        expect(PROTOCOL_VERSION).toBe(declared?.[1]);
    });

    test('rides refreshAccessToken', async () => {
        expect(await refreshAccessToken()).toBe(true);
        const call = theRequest();
        expect(call.url).toEndWith('/v1/auth/refresh');
        expect(call.protocol).toBe(PROTOCOL_VERSION);
    });

    test('rides authFetch', async () => {
        const res = await authFetch('/profile');
        expect(res.status).toBe(200);
        const call = theRequest();
        expect(call.url).toEndWith('/v1/auth/profile');
        expect(call.protocol).toBe(PROTOCOL_VERSION);
    });

    test('rides login', async () => {
        await login({
            email: 'a@example.test',
            password: 'correct horse battery staple',
        });
        const call = theRequest();
        expect(call.url).toEndWith('/v1/auth/login');
        expect(call.protocol).toBe(PROTOCOL_VERSION);
    });

    test('rides register', async () => {
        await register({
            email: 'a@example.test',
            password: 'correct horse battery staple',
        });
        const call = theRequest();
        expect(call.url).toEndWith('/v1/auth/register');
        expect(call.protocol).toBe(PROTOCOL_VERSION);
    });

    test('rides verifyTotpLogin', async () => {
        await verifyTotpLogin('mfa-token', '123456');
        const call = theRequest();
        expect(call.url).toEndWith('/v1/auth/login/verify-totp');
        expect(call.protocol).toBe(PROTOCOL_VERSION);
    });
});
