import { describe, expect, test } from 'bun:test';

import {
    ChangeKind,
    decodeSyncResponse,
    encodeSyncRequest,
    frameRequest,
    parseResponseFrames,
    parseTrailers,
    type SyncResponse,
    utf8,
    WireError,
} from './wire';

// ── A tiny in-test Protobuf encoder for SyncResponse, so decode can be round-tripped
// against an independent writer (the library only ships a request encoder). ────────────

function varint(value: bigint): number[] {
    const out: number[] = [];
    let v = value;
    for (;;) {
        const byte = Number(v & 0x7fn);
        v >>= 7n;
        if (v === 0n) {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

function lenField(field: number, payload: number[]): number[] {
    return [(field << 3) | 2, ...varint(BigInt(payload.length)), ...payload];
}

function varintField(field: number, value: bigint): number[] {
    return [(field << 3) | 0, ...varint(value)];
}

function encBlobRef(ref: {
    hash: string;
    role: string;
    format: string;
    size: bigint;
}): number[] {
    return [
        ...lenField(1, [...utf8(ref.hash)]),
        ...lenField(2, [...utf8(ref.role)]),
        ...lenField(3, [...utf8(ref.format)]),
        ...varintField(4, ref.size),
    ];
}

describe('SyncRequest encoding (hand-verified golden)', () => {
    test('encodes cursor (field 1) + page_size (field 2)', () => {
        const bytes = encodeSyncRequest({
            cursor: new Uint8Array([0x01, 0x02]),
            pageSize: 5,
        });
        expect([...bytes]).toEqual([0x0a, 0x02, 0x01, 0x02, 0x10, 0x05]);
    });

    test('omits proto3 default-valued fields', () => {
        const bytes = encodeSyncRequest({
            cursor: new Uint8Array(),
            pageSize: 0,
        });
        expect([...bytes]).toEqual([]);
    });
});

describe('gRPC-web framing', () => {
    test('frameRequest prepends the 0x00 flag + big-endian length', () => {
        const framed = frameRequest(new Uint8Array([0xaa, 0xbb, 0xcc]));
        expect([...framed]).toEqual([
            0x00, 0x00, 0x00, 0x00, 0x03, 0xaa, 0xbb, 0xcc,
        ]);
    });

    test('parseResponseFrames splits data messages from the trailer frame', () => {
        const data = [0x00, 0x00, 0x00, 0x00, 0x02, 0x12, 0x34];
        const trailerText = 'grpc-status: 0\r\ngrpc-message: ok\r\n';
        const trailerBytes = [...utf8(trailerText)];
        const trailer = [
            0x80,
            0x00,
            0x00,
            0x00,
            trailerBytes.length,
            ...trailerBytes,
        ];
        const { messages, trailers } = parseResponseFrames(
            new Uint8Array([...data, ...trailer]),
        );
        expect(messages).toHaveLength(1);
        expect([...messages[0]]).toEqual([0x12, 0x34]);
        expect(trailers.get('grpc-status')).toBe('0');
        expect(trailers.get('grpc-message')).toBe('ok');
    });

    test('parseResponseFrames rejects a truncated frame', () => {
        expect(() =>
            parseResponseFrames(
                new Uint8Array([0x00, 0x00, 0x00, 0x00, 0x09, 0x01]),
            ),
        ).toThrow(WireError);
    });
});

describe('trailer parsing', () => {
    test('lowercases keys and trims values, ignoring blanks', () => {
        const map = parseTrailers(
            'Grpc-Status: 7\r\nX-Capsule-Error-Code: error.sync.x\r\n\r\n',
        );
        expect(map.get('grpc-status')).toBe('7');
        expect(map.get('x-capsule-error-code')).toBe('error.sync.x');
    });
});

describe('SyncResponse decoding', () => {
    test('decodes a hand-built golden (next_cursor only, no entries)', () => {
        const bytes = new Uint8Array([0x12, 0x01, 0xff]);
        const decoded = decodeSyncResponse(bytes);
        expect(decoded.entries).toHaveLength(0);
        expect([...decoded.nextCursor]).toEqual([0xff]);
    });

    test('round-trips a full entry with nested blob manifest', () => {
        const entryBody = [
            ...lenField(1, [...utf8('album-uuid')]),
            ...varintField(2, 4200000000n), // sync_seq beyond 2^31, exercises bigint
            ...lenField(3, [...utf8('2026-07-11')]),
            ...varintField(4, BigInt(ChangeKind.Created)),
            ...lenField(5, [...utf8('asset-uuid')]),
            ...lenField(6, [0xca, 0xfe]), // opaque manifest_cbor
            ...lenField(7, [0xde, 0xad]), // opaque metadata_blob
            ...lenField(8, [
                ...lenField(
                    1,
                    encBlobRef({
                        hash: 'orig-hash',
                        role: 'original',
                        format: 'video/mp4',
                        size: 9000000000n,
                    }),
                ),
                ...lenField(
                    2,
                    encBlobRef({
                        hash: 'thumb-hash',
                        role: 'derivative',
                        format: 'image/webp',
                        size: 1024n,
                    }),
                ),
            ]),
            ...varintField(9, 1n), // original_held
        ];
        const responseBytes = new Uint8Array([
            ...lenField(1, entryBody),
            ...lenField(2, [0xaa, 0xbb]),
        ]);

        const decoded: SyncResponse = decodeSyncResponse(responseBytes);
        expect([...decoded.nextCursor]).toEqual([0xaa, 0xbb]);
        expect(decoded.entries).toHaveLength(1);
        const e = decoded.entries[0];
        expect(e.albumId).toBe('album-uuid');
        expect(e.syncSeq).toBe(4200000000n);
        expect(e.protocolVersion).toBe('2026-07-11');
        expect(e.kind).toBe(ChangeKind.Created);
        expect(e.assetId).toBe('asset-uuid');
        expect([...e.manifestCbor]).toEqual([0xca, 0xfe]);
        expect([...e.metadataBlob]).toEqual([0xde, 0xad]);
        expect(e.originalHeld).toBe(true);
        expect(e.blobs?.original?.ciphertextHash).toBe('orig-hash');
        expect(e.blobs?.original?.format).toBe('video/mp4');
        expect(e.blobs?.original?.size).toBe(9000000000n);
        expect(e.blobs?.derivatives).toHaveLength(1);
        expect(e.blobs?.derivatives[0].role).toBe('derivative');
    });

    test('skips unknown fields without breaking decode (forward compatibility)', () => {
        // An unknown field 15 (varint) between the known fields must be skipped.
        const bytes = new Uint8Array([
            ...varintField(15, 123n),
            ...lenField(2, [0x01]),
        ]);
        const decoded = decodeSyncResponse(bytes);
        expect([...decoded.nextCursor]).toEqual([0x01]);
    });
});
