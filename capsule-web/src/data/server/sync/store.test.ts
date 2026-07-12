import { describe, expect, test } from 'bun:test';

import {
    CLIENT_MAX_PROTOCOL,
    SyncProtocolError,
    SyncRewindError,
    SyncStore,
    SyncStructuralError,
} from './store';
import { ChangeKind, type SyncEntry } from './wire';

const PROTOCOL = '2026-07-11';
const ALBUM = 'album-a';

function entry(overrides: Partial<SyncEntry> & { syncSeq: bigint }): SyncEntry {
    return {
        albumId: ALBUM,
        protocolVersion: PROTOCOL,
        kind: ChangeKind.Created,
        assetId: `asset-${overrides.syncSeq}`,
        manifestCbor: new Uint8Array(),
        metadataBlob: new Uint8Array(),
        originalHeld: true,
        ...overrides,
    };
}

function cursor(n: number): Uint8Array {
    return new Uint8Array([n]);
}

describe('applyPage — happy path', () => {
    test('applies created entries and advances the cursor', () => {
        const store = new SyncStore();
        store.applyPage(
            [entry({ syncSeq: 1n }), entry({ syncSeq: 2n })],
            cursor(2),
        );
        expect(store.listAssets()).toHaveLength(2);
        expect([...store.cursor]).toEqual([2]);
    });

    test('orders listAssets newest change first (sync_seq desc)', () => {
        const store = new SyncStore();
        // The feed is monotonic per album, so a real page arrives in ascending sync_seq;
        // listAssets then presents it newest-first.
        store.applyPage(
            [
                entry({ syncSeq: 1n, assetId: 'old' }),
                entry({ syncSeq: 3n, assetId: 'mid' }),
                entry({ syncSeq: 5n, assetId: 'new' }),
            ],
            cursor(5),
        );
        expect(store.listAssets().map((a) => a.assetId)).toEqual([
            'new',
            'mid',
            'old',
        ]);
    });

    test('a metadata update replaces the record (e.g. awaiting-original flips)', () => {
        const store = new SyncStore();
        store.applyPage(
            [entry({ syncSeq: 1n, assetId: 'x', originalHeld: false })],
            cursor(1),
        );
        expect(store.listAssets()[0].originalHeld).toBe(false);
        store.applyPage(
            [
                entry({
                    syncSeq: 2n,
                    assetId: 'x',
                    kind: ChangeKind.MetadataUpdated,
                    originalHeld: true,
                }),
            ],
            cursor(2),
        );
        expect(store.listAssets()).toHaveLength(1);
        expect(store.listAssets()[0].originalHeld).toBe(true);
    });

    test('a tombstone removes the asset', () => {
        const store = new SyncStore();
        store.applyPage([entry({ syncSeq: 1n, assetId: 'x' })], cursor(1));
        store.applyPage(
            [entry({ syncSeq: 2n, assetId: 'x', kind: ChangeKind.Deleted })],
            cursor(2),
        );
        expect(store.listAssets()).toHaveLength(0);
    });
});

describe('anti-rewind (download-sync client rule)', () => {
    test('a page regressing below the album high-water is surfaced, not applied', () => {
        const store = new SyncStore();
        store.applyPage([entry({ syncSeq: 5n })], cursor(5));
        expect(() =>
            store.applyPage([entry({ syncSeq: 3n })], cursor(3)),
        ).toThrow(SyncRewindError);
        // Store + cursor untouched by the rejected page.
        expect(store.listAssets()).toHaveLength(1);
        expect([...store.cursor]).toEqual([5]);
    });

    test('equal sync_seq is a rewind (must strictly increase)', () => {
        const store = new SyncStore();
        store.applyPage([entry({ syncSeq: 5n })], cursor(5));
        expect(() =>
            store.applyPage([entry({ syncSeq: 5n })], cursor(5)),
        ).toThrow(SyncRewindError);
    });

    test('high-water is per-album — another album is unaffected', () => {
        const store = new SyncStore();
        store.applyPage([entry({ syncSeq: 9n, albumId: 'a' })], cursor(9));
        // Album b starting at 1 is fine even though a is at 9.
        store.applyPage(
            [entry({ syncSeq: 1n, albumId: 'b', assetId: 'b1' })],
            cursor(10),
        );
        expect(store.listAssets()).toHaveLength(2);
    });

    test('an in-page regression rejects the whole page (no partial application)', () => {
        const store = new SyncStore();
        expect(() =>
            store.applyPage(
                [
                    entry({ syncSeq: 2n, assetId: 'ok' }),
                    entry({ syncSeq: 1n, assetId: 'bad' }),
                ],
                cursor(2),
            ),
        ).toThrow(SyncRewindError);
        expect(store.listAssets()).toHaveLength(0);
        expect([...store.cursor]).toEqual([]);
    });
});

describe('forward-version rejection (download-sync client rule)', () => {
    test('an entry above the client max protocol is refused without partial apply', () => {
        const store = new SyncStore();
        expect(() =>
            store.applyPage(
                [
                    entry({ syncSeq: 1n, assetId: 'ok' }),
                    entry({
                        syncSeq: 2n,
                        assetId: 'future',
                        protocolVersion: '2099-01-01',
                    }),
                ],
                cursor(2),
            ),
        ).toThrow(SyncProtocolError);
        expect(store.listAssets()).toHaveLength(0);
        expect([...store.cursor]).toEqual([]);
    });

    test('the client max protocol itself is accepted', () => {
        const store = new SyncStore();
        store.applyPage(
            [entry({ syncSeq: 1n, protocolVersion: CLIENT_MAX_PROTOCOL })],
            cursor(1),
        );
        expect(store.listAssets()).toHaveLength(1);
    });
});

describe('structural validation', () => {
    test('an unspecified ChangeKind is rejected', () => {
        const store = new SyncStore();
        expect(() =>
            store.applyPage(
                [entry({ syncSeq: 1n, kind: ChangeKind.Unspecified })],
                cursor(1),
            ),
        ).toThrow(SyncStructuralError);
        expect(store.listAssets()).toHaveLength(0);
    });
});

describe('album summaries', () => {
    test('counts live members and drops an album once emptied', () => {
        const store = new SyncStore();
        store.applyPage(
            [
                entry({ syncSeq: 1n, albumId: 'a', assetId: 'a1' }),
                entry({ syncSeq: 2n, albumId: 'a', assetId: 'a2' }),
                entry({ syncSeq: 3n, albumId: 'b', assetId: 'b1' }),
            ],
            cursor(3),
        );
        expect(store.getAlbum('a')?.assetCount).toBe(2);
        expect(store.getAlbum('b')?.assetCount).toBe(1);
        expect(store.assetsForAlbum('a').map((x) => x.assetId)).toEqual([
            'a2',
            'a1',
        ]);

        store.applyPage(
            [
                entry({
                    syncSeq: 4n,
                    albumId: 'a',
                    assetId: 'a1',
                    kind: ChangeKind.Deleted,
                }),
                entry({
                    syncSeq: 5n,
                    albumId: 'a',
                    assetId: 'a2',
                    kind: ChangeKind.Deleted,
                }),
            ],
            cursor(5),
        );
        expect(store.getAlbum('a')).toBeNull();
        expect(store.albums().map((s) => s.albumId)).toEqual(['b']);
    });
});

describe('snapshot / restore', () => {
    test('round-trips state, cursor and high-water; rewind still detected after restore', () => {
        const store = new SyncStore();
        store.applyPage(
            [
                entry({
                    syncSeq: 7n,
                    albumId: 'a',
                    assetId: 'a1',
                    originalHeld: false,
                }),
                entry({
                    syncSeq: 2n,
                    albumId: 'b',
                    assetId: 'b1',
                    blobs: {
                        original: {
                            ciphertextHash: 'h',
                            role: 'original',
                            format: 'image/jpeg',
                            size: 42n,
                        },
                        derivatives: [],
                    },
                }),
            ],
            cursor(9),
        );
        const snap = store.snapshot();

        const restored = new SyncStore();
        restored.restore(snap);
        expect(restored.listAssets()).toHaveLength(2);
        expect([...restored.cursor]).toEqual([9]);
        expect(restored.getAlbum('a')?.assetCount).toBe(1);
        expect(
            restored.listAssets().find((a) => a.assetId === 'b1')?.original
                ?.size,
        ).toBe(42n);
        // High-water survived: album a is at 7, so a regressing page is still refused.
        expect(() =>
            restored.applyPage(
                [entry({ syncSeq: 4n, albumId: 'a' })],
                cursor(4),
            ),
        ).toThrow(SyncRewindError);
    });

    test('snapshot is JSON-serializable (bigints as decimal strings)', () => {
        const store = new SyncStore();
        store.applyPage([entry({ syncSeq: 9000000000n })], cursor(1));
        const json = JSON.stringify(store.snapshot());
        expect(json).toContain('9000000000');
    });
});
