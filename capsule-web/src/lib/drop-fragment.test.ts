// Upload-link fragment parsing (slice S-D3): the route-state logic that turns the URL fragment
// into a Drop Key (+ optional passphrase params), or a single generic "incomplete" state.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

import { parseDropFragment } from './drop-fragment';

// A real, well-formed Drop Key public half (1216 bytes) taken from the KAT fixture, re-encoded as
// base64url (the fragment encoding) so the parser is exercised against genuine key material.
const fixture = JSON.parse(
    readFileSync(
        new URL('../generated/drop-kat.json', import.meta.url),
        'utf8',
    ),
) as { dropPubkeyB64: string };

const dropPubkeyBytes = new Uint8Array(
    Buffer.from(fixture.dropPubkeyB64, 'base64'),
);
const b64url = Buffer.from(dropPubkeyBytes)
    .toString('base64')
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');

describe('parseDropFragment', () => {
    test('parses a plain link: just the Drop Key', () => {
        const link = parseDropFragment(b64url);
        expect(link).not.toBeNull();
        expect(link?.passphrase).toBeNull();
        expect(new Uint8Array(link?.dropPubkey ?? [])).toEqual(dropPubkeyBytes);
    });

    test('parses a passphrase-gated link: Drop Key + Argon2id salt/params', () => {
        const link = parseDropFragment(`${b64url}~65536~3~1~9a9a9a9a9a9a9a9a`);
        expect(link).not.toBeNull();
        expect(new Uint8Array(link?.dropPubkey ?? [])).toEqual(dropPubkeyBytes);
        expect(link?.passphrase).toEqual({
            saltHex: '9a9a9a9a9a9a9a9a',
            memKib: 65536,
            tCost: 3,
            pCost: 1,
        });
    });

    test('tolerates a leading whitespace / empty fragment as incomplete', () => {
        expect(parseDropFragment('')).toBeNull();
        expect(parseDropFragment('   ')).toBeNull();
    });

    test('rejects a wrong-length / undecodable Drop Key', () => {
        expect(parseDropFragment('not-a-real-key')).toBeNull();
        // A valid base64url but too short to be a Drop Key.
        expect(parseDropFragment('AAAA')).toBeNull();
    });

    test('rejects malformed passphrase params', () => {
        // Wrong field count.
        expect(parseDropFragment(`${b64url}~65536~3`)).toBeNull();
        // Non-positive / non-integer cost.
        expect(parseDropFragment(`${b64url}~0~3~1~9a9a`)).toBeNull();
        expect(parseDropFragment(`${b64url}~65536~x~1~9a9a`)).toBeNull();
        // Non-hex salt.
        expect(parseDropFragment(`${b64url}~65536~3~1~zzzz`)).toBeNull();
    });
});
