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
    passphrase: string;
    wrongPassphrase: string;
    linkOnlyWrappedB64: string;
    passphraseWrappedB64: string;
    plaintextB64: string;
    ciphertextB64: string;
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
