// Cross-language share-link Known-Answer Test.
//
// The Rust issuer crypto (`capsule_core::sharing`) seals a fixture via `cargo xtask share-kat`;
// this test reopens it through the `capsule-wasm` browser surface. Passing proves the two
// implementations are byte-identical across the language boundary:
//   • byte-exact plaintext recovery (link-only AND passphrase-wrapped),
//   • wrong-passphrase refusal (scenario #42 — the Argon2id backstop, client-side),
//   • wrong-fragment refusal (the URL secret is load-bearing),
//   • tampered-ciphertext refusal (the STREAM AEAD tag),
//   • passphrase-required detection without a passphrase.
//
// The fixture + wasm are build-time generated (`mise run share-kat` / `build-wasm`), so this test
// is a pure function of the Rust source — no committed binaries.

import { beforeAll, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

import {
    decryptShareBlob,
    initShareWasm,
    loadShare,
    openShare,
    type ShareLoad,
    shareIsPassphraseProtected,
    shareOpenCode,
} from './share-open';

/** One link as the server serves it (#614): the `GET /s/{id}` body, every blob octet stream the
 *  link names, and the wrapped-secret octets when there are any. */
interface ServedLink {
    metadata: { metadata_hash: string; passphrase_protected: boolean };
    blobs: Record<string, string>;
    wrappedSecretB64: string | null;
}

interface Fixture {
    served: { linkOnly: ServedLink; passphrase: ServedLink };
    opaqueIdHex: string;
    fragmentHex: string;
    wrongFragmentHex: string;
    fileId: string;
    amkVersion: number;
    noncePrefixHex: string;
    contentType: string;
    passphrase: string;
    wrongPassphrase: string;
    linkOnlyWrappedB64: string;
    passphraseWrappedB64: string;
    plaintextB64: string;
    ciphertextB64: string;
    album: AlbumFixture;
}

/** One album asset as the fixture serves it. */
interface AlbumAsset {
    fileId: string;
    noncePrefixHex: string;
    ciphertextB64: string;
}

/** The album-scoped grant over a derived asset and an adopted (wrapped-key) drop (#598). */
interface AlbumFixture {
    served: ServedLink;
    linkOnlyWrappedB64: string;
    amkVersion: number;
    derived: AlbumAsset;
    wrapped: AlbumAsset & {
        wrappedFileKeyHex: string;
        foreignWrappedFileKeyHex: string;
    };
}

const fixture: Fixture = JSON.parse(
    readFileSync(
        new URL('../generated/share-kat.json', import.meta.url),
        'utf8',
    ),
);

const bytes = (b64: string): Uint8Array =>
    new Uint8Array(Buffer.from(b64, 'base64'));

/** Capture the stable machine code of a thrown wasm error. */
function codeOf(fn: () => unknown): string {
    try {
        fn();
    } catch (error) {
        return shareOpenCode(error);
    }
    throw new Error('expected the call to throw, but it did not');
}

beforeAll(async () => {
    // In bun, initialize the wasm module from bytes (the browser fetches its sibling .wasm).
    await initShareWasm({
        module_or_path: readFileSync(
            new URL('../generated/wasm/capsule_wasm_bg.wasm', import.meta.url),
        ),
    });
});

describe('share-link cross-language KAT', () => {
    test('passphrase protection is detected client-side from the opaque material', () => {
        expect(shareIsPassphraseProtected(fixture.linkOnlyWrappedB64)).toBe(
            false,
        );
        expect(shareIsPassphraseProtected(fixture.passphraseWrappedB64)).toBe(
            true,
        );
    });

    test('link-only: opens with the fragment and recovers the exact plaintext', () => {
        const scope = openShare(
            fixture.linkOnlyWrappedB64,
            fixture.opaqueIdHex,
            fixture.fragmentHex,
            null,
        );
        expect(scope.scopeKind()).toBe('asset');
        const plaintext = scope.decryptBlob(
            fixture.fileId,
            fixture.amkVersion,
            fixture.noncePrefixHex,
            bytes(fixture.ciphertextB64),
        );
        expect(new Uint8Array(plaintext)).toEqual(bytes(fixture.plaintextB64));
    });

    test('end-to-end asset render: the viewer helper decrypts a byte-exact image blob', () => {
        // Mirrors the guest viewer's render path: open the scope, then decrypt the served
        // ciphertext through the SAME `decryptShareBlob` helper the route uses, fed the serve
        // response's crypto params (asset id, AMK epoch, nonce prefix).
        const scope = openShare(
            fixture.linkOnlyWrappedB64,
            fixture.opaqueIdHex,
            fixture.fragmentHex,
            null,
        );
        const plaintext = decryptShareBlob(
            scope,
            {
                assetId: fixture.fileId,
                amkVersion: fixture.amkVersion,
                noncePrefixHex: fixture.noncePrefixHex,
            },
            bytes(fixture.ciphertextB64),
        );
        // Byte-exact plaintext recovery, end to end (fixture ciphertext → decrypt → plaintext).
        expect(new Uint8Array(plaintext)).toEqual(bytes(fixture.plaintextB64));
        // The decrypted bytes wrap into a content-typed Blob — what the viewer turns into a
        // thumbnail object URL — and lead with the PNG signature the fixture seals.
        const blob = new Blob([plaintext], { type: fixture.contentType });
        expect(blob.type).toBe('image/png');
        expect(blob.size).toBe(bytes(fixture.plaintextB64).length);
        expect(Array.from(plaintext.slice(0, 8))).toEqual([
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
        ]);
        scope.free();
    });

    test('passphrase: unwraps client-side and recovers the exact plaintext', () => {
        const scope = openShare(
            fixture.passphraseWrappedB64,
            fixture.opaqueIdHex,
            fixture.fragmentHex,
            fixture.passphrase,
        );
        const plaintext = scope.decryptBlob(
            fixture.fileId,
            fixture.amkVersion,
            fixture.noncePrefixHex,
            bytes(fixture.ciphertextB64),
        );
        expect(new Uint8Array(plaintext)).toEqual(bytes(fixture.plaintextB64));
    });

    test('wrong passphrase is refused (scenario #42 — Argon2id backstop)', () => {
        expect(
            codeOf(() =>
                openShare(
                    fixture.passphraseWrappedB64,
                    fixture.opaqueIdHex,
                    fixture.fragmentHex,
                    fixture.wrongPassphrase,
                ),
            ),
        ).toBe('wrong_secret');
    });

    test('a missing passphrase on a protected link is refused, not silently served', () => {
        expect(
            codeOf(() =>
                openShare(
                    fixture.passphraseWrappedB64,
                    fixture.opaqueIdHex,
                    fixture.fragmentHex,
                    null,
                ),
            ),
        ).toBe('passphrase_required');
    });

    test('a wrong URL fragment secret does not open the scope', () => {
        expect(
            codeOf(() =>
                openShare(
                    fixture.linkOnlyWrappedB64,
                    fixture.opaqueIdHex,
                    fixture.wrongFragmentHex,
                    null,
                ),
            ),
        ).toBe('wrong_secret');
    });

    test('a tampered ciphertext blob is rejected by the AEAD tag', () => {
        const scope = openShare(
            fixture.linkOnlyWrappedB64,
            fixture.opaqueIdHex,
            fixture.fragmentHex,
            null,
        );
        const tampered = bytes(fixture.ciphertextB64);
        tampered[0] ^= 0x01;
        expect(
            codeOf(() =>
                scope.decryptBlob(
                    fixture.fileId,
                    fixture.amkVersion,
                    fixture.noncePrefixHex,
                    tampered,
                ),
            ),
        ).toBe('tampered');
    });
});

// #598: an album-scoped grant covers an adopted web-upload drop, whose key a guest chose and
// no AMK derives. The viewer recovers it from the served `wrapped_file_key`.
describe('album-scope share over an adopted (wrapped-key) drop', () => {
    const album = fixture.album;
    const openAlbum = () =>
        openShare(
            album.linkOnlyWrappedB64,
            fixture.opaqueIdHex,
            fixture.fragmentHex,
            null,
        );

    test('a derived album asset still decrypts with no key mode given', () => {
        const scope = openAlbum();
        expect(scope.scopeKind()).toBe('album');
        const plaintext = decryptShareBlob(
            scope,
            {
                assetId: album.derived.fileId,
                amkVersion: album.amkVersion,
                noncePrefixHex: album.derived.noncePrefixHex,
            },
            bytes(album.derived.ciphertextB64),
        );
        expect(new Uint8Array(plaintext)).toEqual(bytes(fixture.plaintextB64));
        scope.free();
    });

    test('a wrapped asset decrypts byte-exact through the viewer helper', () => {
        const scope = openAlbum();
        const plaintext = decryptShareBlob(
            scope,
            {
                assetId: album.wrapped.fileId,
                amkVersion: album.amkVersion,
                noncePrefixHex: album.wrapped.noncePrefixHex,
                keyMode: 'wrapped',
                wrappedFileKeyHex: album.wrapped.wrappedFileKeyHex,
            },
            bytes(album.wrapped.ciphertextB64),
        );
        expect(new Uint8Array(plaintext)).toEqual(bytes(fixture.plaintextB64));
        scope.free();
    });

    test('the same asset read as derived fails its AEAD open, as before the fix', () => {
        const scope = openAlbum();
        expect(
            codeOf(() =>
                decryptShareBlob(
                    scope,
                    {
                        assetId: album.wrapped.fileId,
                        amkVersion: album.amkVersion,
                        noncePrefixHex: album.wrapped.noncePrefixHex,
                    },
                    bytes(album.wrapped.ciphertextB64),
                ),
            ),
        ).toBe('tampered');
        scope.free();
    });

    test('a wrapped key sealed under another AMK fails closed as tampered', () => {
        const scope = openAlbum();
        expect(
            codeOf(() =>
                scope.decryptBlob(
                    album.wrapped.fileId,
                    album.amkVersion,
                    album.wrapped.noncePrefixHex,
                    bytes(album.wrapped.ciphertextB64),
                    album.wrapped.foreignWrappedFileKeyHex,
                ),
            ),
        ).toBe('tampered');
        scope.free();
    });

    test('an epoch the grant holds no AMK for is scope_unavailable', () => {
        const scope = openAlbum();
        expect(
            codeOf(() =>
                scope.decryptBlob(
                    album.wrapped.fileId,
                    album.amkVersion + 1,
                    album.wrapped.noncePrefixHex,
                    bytes(album.wrapped.ciphertextB64),
                    album.wrapped.wrappedFileKeyHex,
                ),
            ),
        ).toBe('scope_unavailable');
        scope.free();
    });

    test('a wrapped key of the wrong length is malformed', () => {
        const scope = openAlbum();
        expect(
            codeOf(() =>
                scope.decryptBlob(
                    album.wrapped.fileId,
                    album.amkVersion,
                    album.wrapped.noncePrefixHex,
                    bytes(album.wrapped.ciphertextB64),
                    album.wrapped.wrappedFileKeyHex.slice(2),
                ),
            ),
        ).toBe('malformed');
        scope.free();
    });

    test('a key mode that disagrees with the wrapped key is malformed', () => {
        const scope = openAlbum();
        const base = {
            assetId: album.wrapped.fileId,
            amkVersion: album.amkVersion,
            noncePrefixHex: album.wrapped.noncePrefixHex,
        };
        const ciphertext = bytes(album.wrapped.ciphertextB64);
        expect(
            codeOf(() =>
                decryptShareBlob(
                    scope,
                    { ...base, keyMode: 'wrapped' },
                    ciphertext,
                ),
            ),
        ).toBe('malformed');
        expect(
            codeOf(() =>
                decryptShareBlob(
                    scope,
                    {
                        ...base,
                        keyMode: 'derived',
                        wrappedFileKeyHex: album.wrapped.wrappedFileKeyHex,
                    },
                    ciphertext,
                ),
            ),
        ).toBe('malformed');
        scope.free();
    });
});

// #614: the viewer's fetch sequence against a link in the server's own wire shapes — the
// `SharedMetadataResponse` JSON, the sealed record and ciphertext as octets, the wrapped secret
// as octets — published by the same `publish_share` a client runs before `POST /v1/shares`.
describe('loading a served share link', () => {
    const BASE = 'https://home.example';
    const opaqueId = fixture.opaqueIdHex;

    /** A stand-in for the server: answers exactly the three `/s/…` routes, `404` otherwise —
     *  the server's one refusal — and records what was asked for. */
    function serve(link: ServedLink, requested: string[] = []): typeof fetch {
        const handler = async (input: RequestInfo | URL) => {
            const url = String(input);
            requested.push(url.slice(BASE.length));
            const prefix = `${BASE}/s/${opaqueId}`;
            if (url === prefix) return Response.json(link.metadata);
            if (url === `${prefix}/wrapped-secret`) {
                return link.wrappedSecretB64 === null
                    ? new Response(null, { status: 404 })
                    : new Response(bytes(link.wrappedSecretB64));
            }
            const hash = url.startsWith(`${prefix}/blob/`)
                ? url.slice(`${prefix}/blob/`.length)
                : undefined;
            const blob = hash === undefined ? undefined : link.blobs[hash];
            return blob === undefined
                ? new Response(null, { status: 404 })
                : new Response(bytes(blob));
        };
        return handler as typeof fetch;
    }

    /** Open the loaded scope and decrypt every listed asset through the viewer's helper. */
    async function decryptAll(
        loaded: Extract<ShareLoad, { wrapped: string }>,
        link: ServedLink,
        passphrase: string | null,
    ): Promise<Uint8Array[]> {
        const scope = openShare(
            loaded.wrapped,
            opaqueId,
            fixture.fragmentHex,
            passphrase,
        );
        try {
            return loaded.meta.assets.map((asset) =>
                decryptShareBlob(
                    scope,
                    asset,
                    bytes(link.blobs[asset.contentHash] ?? ''),
                ),
            );
        } finally {
            scope.free();
        }
    }

    test('a link-only link opens from the record alone and never asks for a wrapped secret', async () => {
        const link = fixture.served.linkOnly;
        const requested: string[] = [];
        const loaded = await loadShare(
            serve(link, requested),
            BASE,
            opaqueId,
            fixture.fragmentHex,
        );
        expect(loaded.kind).toBe('open');
        if (loaded.kind !== 'open') return;
        expect(requested).toEqual([
            `/s/${opaqueId}`,
            `/s/${opaqueId}/blob/${link.metadata.metadata_hash}`,
        ]);
        expect(loaded.meta.expiresAt).toBe('2030-01-01T00:00:00Z');
        expect(loaded.meta.assets).toHaveLength(1);
        const [asset] = loaded.meta.assets;
        expect(asset?.assetId).toBe(fixture.fileId);
        expect(asset?.contentType).toBe('image/png');
        expect(asset?.size).toBe(bytes(fixture.plaintextB64).length);
        expect(asset?.keyMode).toBe('derived');
        expect(asset?.wrappedFileKeyHex).toBeUndefined();

        const [plaintext] = await decryptAll(loaded, link, null);
        expect(plaintext).toEqual(bytes(fixture.plaintextB64));
    });

    test('a passphrase link fetches its wrapped secret as octets and opens only with the passphrase', async () => {
        const link = fixture.served.passphrase;
        const requested: string[] = [];
        const loaded = await loadShare(
            serve(link, requested),
            BASE,
            opaqueId,
            fixture.fragmentHex,
        );
        expect(loaded.kind).toBe('passphrase');
        if (loaded.kind !== 'passphrase') return;
        expect(requested).toEqual([
            `/s/${opaqueId}`,
            `/s/${opaqueId}/blob/${link.metadata.metadata_hash}`,
            `/s/${opaqueId}/wrapped-secret`,
        ]);
        expect(shareIsPassphraseProtected(loaded.wrapped)).toBe(true);
        expect(
            codeOf(() =>
                openShare(loaded.wrapped, opaqueId, fixture.fragmentHex, null),
            ),
        ).toBe('passphrase_required');

        const [plaintext] = await decryptAll(loaded, link, fixture.passphrase);
        expect(plaintext).toEqual(bytes(fixture.plaintextB64));
    });

    test('an album link carries each asset key mode, and both modes decrypt', async () => {
        const link = fixture.album.served;
        const loaded = await loadShare(
            serve(link),
            BASE,
            opaqueId,
            fixture.fragmentHex,
        );
        expect(loaded.kind).toBe('open');
        if (loaded.kind !== 'open') return;
        expect(loaded.meta.assets.map((a) => a.keyMode)).toEqual([
            'derived',
            'wrapped',
        ]);
        expect(loaded.meta.assets[1]?.wrappedFileKeyHex).toBe(
            fixture.album.wrapped.wrappedFileKeyHex,
        );
        for (const plaintext of await decryptAll(loaded, link, null)) {
            expect(plaintext).toEqual(bytes(fixture.plaintextB64));
        }
    });

    test('a wrong fragment is the same unavailable answer as a missing link', async () => {
        const link = fixture.served.linkOnly;
        expect(
            await loadShare(
                serve(link),
                BASE,
                opaqueId,
                fixture.wrongFragmentHex,
            ),
        ).toEqual({ kind: 'unavailable' });
        const gone: ServedLink = { ...link, blobs: {} };
        expect(
            await loadShare(serve(gone), BASE, opaqueId, fixture.fragmentHex),
        ).toEqual({ kind: 'unavailable' });
        const notFound = (async () =>
            new Response(null, { status: 404 })) as unknown as typeof fetch;
        expect(
            await loadShare(notFound, BASE, opaqueId, fixture.fragmentHex),
        ).toEqual({ kind: 'unavailable' });
    });

    test('a passphrase link whose wrapped secret is not served is unavailable', async () => {
        const link: ServedLink = {
            ...fixture.served.passphrase,
            wrappedSecretB64: null,
        };
        expect(
            await loadShare(serve(link), BASE, opaqueId, fixture.fragmentHex),
        ).toEqual({ kind: 'unavailable' });
    });

    test('a link-only record served as passphrase-free but holding no scope is malformed', async () => {
        // The passphrase record carries no scope; claiming it needs none must not open it.
        const link: ServedLink = {
            ...fixture.served.passphrase,
            metadata: {
                ...fixture.served.passphrase.metadata,
                passphrase_protected: false,
            },
        };
        let thrown: unknown;
        try {
            await loadShare(serve(link), BASE, opaqueId, fixture.fragmentHex);
        } catch (error) {
            thrown = error;
        }
        expect(shareOpenCode(thrown)).toBe('malformed');
    });

    test('a peer that does not host the share points at its home server', async () => {
        const peer = (async () =>
            Response.json(
                { home_server: 'home.example' },
                { status: 421 },
            )) as unknown as typeof fetch;
        expect(
            await loadShare(peer, BASE, opaqueId, fixture.fragmentHex),
        ).toEqual({ kind: 'redirect', homeServer: 'home.example' });
    });
});
