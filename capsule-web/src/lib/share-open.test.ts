// Cross-language share-link Known-Answer Test (slice S-E1).
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
    openShare,
    shareIsPassphraseProtected,
    shareOpenCode,
} from './share-open';

interface Fixture {
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
