// Cross-language guest-drop Known-Answer Test (slice S-D3) — the reverse of the share-link KAT.
//
// Here the BROWSER seals. `cargo xtask drop-kat` pins the byte-identical Rust `seal_drop_derand`
// output (and the passphrase abuse-gate proof) for fixed inputs; this test drives the
// `capsule-wasm` seal surface with those same inputs and asserts it reproduces the fixture exactly.
// Passing proves the two implementations are byte-identical across the language boundary:
//   • the browser's derandomized seal == Rust's `seal_drop_derand` (descriptor + ciphertext),
//   • the browser's Argon2id passphrase proof == the server's stored verifier,
//   • the production (random) seal is structurally well-formed and hash-bound,
//   • the surface is contribute-only — no drop open/decrypt export exists.
//
// The very bytes proven here are what `capsule-core/tests/drop_adopt_kat.rs` adopts (same fixed
// inputs), closing E2E case 13's browser half at the level the repo runs locally (a live-browser
// run is still owed, as for S-E1/S-D6).
//
// The fixture + wasm are build-time generated (`mise run drop-kat` / `build-wasm`), so this test is
// a pure function of the Rust source — no committed binaries.

import { beforeAll, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

import * as wasm from '@/generated/wasm/capsule_wasm';
import {
    dropPassphraseProof,
    initDropWasm,
    sealDrop,
    sealDropDerand,
} from './drop-seal';
import { sha256Hex } from './drop-upload';

interface Fixture {
    dropSeedHex: string;
    dropPubkeyB64: string;
    contentType: string;
    plaintextB64: string;
    kHex: string;
    noncePrefixHex: string;
    eseedHex: string;
    blobNonceHex: string;
    descriptor: {
        contentType: string;
        plaintextSize: number;
        chunkSize: number;
        noncePrefixHex: string;
        ciphertextHashHex: string;
        kemCtB64: string;
    };
    ciphertextB64: string;
    passphrase: string;
    wrongPassphrase: string;
    passphraseSaltHex: string;
    passphraseMemKib: number;
    passphraseTCost: number;
    passphrasePCost: number;
    expectedProofHex: string;
}

const fixture: Fixture = JSON.parse(
    readFileSync(
        new URL('../generated/drop-kat.json', import.meta.url),
        'utf8',
    ),
);

const bytes = (b64: string): Uint8Array =>
    new Uint8Array(Buffer.from(b64, 'base64'));

beforeAll(async () => {
    await initDropWasm({
        module_or_path: readFileSync(
            new URL('../generated/wasm/capsule_wasm_bg.wasm', import.meta.url),
        ),
    });
});

describe('guest-drop cross-language KAT', () => {
    test('the browser derand seal reproduces the Rust seal byte-for-byte', () => {
        const sealed = sealDropDerand(
            bytes(fixture.plaintextB64),
            bytes(fixture.dropPubkeyB64),
            fixture.contentType,
            fixture.kHex,
            fixture.noncePrefixHex,
            fixture.eseedHex,
            fixture.blobNonceHex,
        );
        try {
            expect(sealed.contentType()).toBe(fixture.descriptor.contentType);
            expect(sealed.plaintextSize()).toBe(
                fixture.descriptor.plaintextSize,
            );
            expect(sealed.chunkSize()).toBe(fixture.descriptor.chunkSize);
            expect(sealed.noncePrefixHex()).toBe(
                fixture.descriptor.noncePrefixHex,
            );
            expect(sealed.ciphertextHashHex()).toBe(
                fixture.descriptor.ciphertextHashHex,
            );
            // The encapsulated key and the STREAM ciphertext match the Rust core exactly.
            expect(sealed.kemCtB64()).toBe(fixture.descriptor.kemCtB64);
            expect(new Uint8Array(sealed.ciphertext())).toEqual(
                bytes(fixture.ciphertextB64),
            );
        } finally {
            sealed.free();
        }
    });

    test('the production (random) seal is well-formed and hash-bound', async () => {
        const plaintext = bytes(fixture.plaintextB64);
        const sealed = sealDrop(
            plaintext,
            bytes(fixture.dropPubkeyB64),
            fixture.contentType,
        );
        try {
            // The ciphertext length depends only on the plaintext, so it matches the fixture;
            // its bytes differ (a fresh random K), proving the CSPRNG path is live.
            const ciphertext = new Uint8Array(sealed.ciphertext());
            expect(ciphertext.length).toBe(bytes(fixture.ciphertextB64).length);
            expect(ciphertext).not.toEqual(bytes(fixture.ciphertextB64));
            // The kem_ct length is fixed by the suite (matches the deterministic fixture).
            expect(bytes(sealed.kemCtB64()).length).toBe(
                bytes(fixture.descriptor.kemCtB64).length,
            );
            // The descriptor commits to the exact ciphertext bytes.
            expect(sealed.ciphertextHashHex()).toBe(
                await sha256Hex(ciphertext),
            );
            expect(sealed.plaintextSize()).toBe(plaintext.length);
        } finally {
            sealed.free();
        }
    });

    test('the passphrase proof matches the server verifier byte-for-byte', () => {
        const proof = dropPassphraseProof(
            fixture.passphrase,
            fixture.passphraseSaltHex,
            fixture.passphraseMemKib,
            fixture.passphraseTCost,
            fixture.passphrasePCost,
        );
        expect(proof).toBe(fixture.expectedProofHex);

        // A wrong passphrase derives a different proof — the server would refuse it.
        const wrong = dropPassphraseProof(
            fixture.wrongPassphrase,
            fixture.passphraseSaltHex,
            fixture.passphraseMemKib,
            fixture.passphraseTCost,
            fixture.passphrasePCost,
        );
        expect(wrong).not.toBe(fixture.expectedProofHex);
    });

    test('the surface is contribute-only: no drop open/decrypt export exists', () => {
        const exported = Object.keys(wasm).map((k) => k.toLowerCase());
        for (const name of exported) {
            const isDropOpener =
                name.includes('drop') &&
                (name.includes('open') ||
                    name.includes('decrypt') ||
                    name.includes('decapsulat') ||
                    name.includes('adopt'));
            expect(isDropOpener).toBe(false);
        }
    });
});
