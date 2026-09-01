import { describe, expect, test } from 'bun:test';

import {
    BlobRole,
    ChangeKind,
    decodeSyncResponse,
    encodeSyncRequest,
    SyncDecodeError,
} from './wire';

/** A page body in exactly the shape `capsule-server/openapi.json` declares. */
function page(overrides: Record<string, unknown> = {}) {
    return {
        entries: [
            {
                asset_id: '018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61',
                album_id: '018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e00',
                protocol_version: '2026-01-01',
                sync_seq: 7,
                change: 'created',
                manifest_cbor: 'AQIDBA==',
                metadata_blob: 'a'.repeat(64),
                blobs: [
                    { role: 'original', hash: 'b'.repeat(64), size: 4096 },
                    { role: 'derivative', hash: 'c'.repeat(64), size: 512 },
                    { role: 'derivative', hash: 'd'.repeat(64), size: 256 },
                    { role: 'provenance', hash: 'e'.repeat(64), size: 128 },
                ],
                original_held: true,
                changed_at: '2026-09-01T00:00:00Z',
            },
        ],
        next_cursor: 'opaque-cursor',
        has_more: false,
        ...overrides,
    };
}

describe('decodeSyncResponse', () => {
    test('reads a page in the shape the served document declares', () => {
        const decoded = decodeSyncResponse(page());

        expect(decoded.nextCursor).toBe('opaque-cursor');
        expect(decoded.hasMore).toBe(false);
        expect(decoded.entries).toHaveLength(1);

        const entry = decoded.entries[0];
        expect(entry.assetId).toBe('018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61');
        expect(entry.syncSeq).toBe(7n);
        expect(entry.kind).toBe(ChangeKind.Created);
        expect(entry.originalHeld).toBe(true);
        expect(entry.changedAt).toBe('2026-09-01T00:00:00Z');
    });

    test('carries the signed manifest and the metadata address opaquely', () => {
        // The browser holds no album keys. These must arrive as they were sent and be
        // touched by nothing on the way through.
        const decoded = decodeSyncResponse(page());
        expect(decoded.entries[0].manifestCbor).toBe('AQIDBA==');
        expect(decoded.entries[0].metadataBlob).toBe('a'.repeat(64));
    });

    test('groups blobs into the original and its derivatives', () => {
        const { blobs } = decodeSyncResponse(page()).entries[0];

        expect(blobs.original?.role).toBe(BlobRole.Original);
        expect(blobs.original?.size).toBe(4096n);
        expect(blobs.derivatives).toHaveLength(2);
        expect(blobs.derivatives.map((ref) => ref.size)).toEqual([512n, 256n]);
    });

    test('a tombstone carries no manifest and no metadata', () => {
        const body = page({
            entries: [
                {
                    ...page().entries[0],
                    change: 'deleted',
                    manifest_cbor: null,
                    metadata_blob: null,
                    blobs: [],
                    original_held: false,
                },
            ],
        });
        const entry = decodeSyncResponse(body).entries[0];

        expect(entry.kind).toBe(ChangeKind.Deleted);
        expect(entry.manifestCbor).toBeUndefined();
        expect(entry.metadataBlob).toBeUndefined();
        expect(entry.blobs.original).toBeUndefined();
    });

    test('an empty page still carries a cursor', () => {
        // The server re-mints the position the client arrived with, so a client never has to
        // decide whether to keep its old cursor.
        const decoded = decodeSyncResponse(
            page({ entries: [], next_cursor: 'same-place' }),
        );
        expect(decoded.entries).toHaveLength(0);
        expect(decoded.nextCursor).toBe('same-place');
    });

    test('a body missing a required field is refused, not silently undefined', () => {
        // The store's anti-rewind check compares sequence numbers, and `undefined` compares
        // false against everything — so a partial body has to fail here rather than there.
        const body = page();
        delete (body.entries[0] as Record<string, unknown>).sync_seq;
        expect(() => decodeSyncResponse(body)).toThrow(SyncDecodeError);

        expect(() => decodeSyncResponse({ entries: [] })).toThrow(
            SyncDecodeError,
        );
        expect(() => decodeSyncResponse(null)).toThrow(SyncDecodeError);
    });

    test('an unknown change kind or blob role is refused', () => {
        // Both are closed enums on the wire. A value outside them means the client is talking
        // to a server it does not understand, and guessing is how a tombstone gets applied as
        // a create.
        expect(() =>
            decodeSyncResponse(
                page({
                    entries: [{ ...page().entries[0], change: 'archived' }],
                }),
            ),
        ).toThrow(SyncDecodeError);
        expect(() =>
            decodeSyncResponse(
                page({
                    entries: [
                        {
                            ...page().entries[0],
                            blobs: [
                                {
                                    role: 'sidecar',
                                    hash: 'f'.repeat(64),
                                    size: 1,
                                },
                            ],
                        },
                    ],
                }),
            ),
        ).toThrow(SyncDecodeError);
    });
});

describe('encodeSyncRequest', () => {
    test('a first sync sends no parameters at all', () => {
        // "I have seen nothing" and "resume after position 0" are one request, and the server
        // reads an absent cursor as the beginning.
        expect(encodeSyncRequest({})).toBe('');
    });

    test('a resumed sync carries the cursor and the page-size hint', () => {
        expect(encodeSyncRequest({ cursor: 'abc', pageSize: 256 })).toBe(
            '?cursor=abc&page_size=256',
        );
    });

    test('a cursor is percent-encoded, so an opaque token survives the query string', () => {
        // The cursor is base64url + a MAC; the encoder must not assume it is URL-safe.
        expect(encodeSyncRequest({ cursor: 'a+b/c=' })).toBe(
            '?cursor=a%2Bb%2Fc%3D',
        );
    });
});
