import { describe, expect, test } from 'bun:test';

import { ServerGateway } from './server-gateway';
import { MemoryPersistence } from './sync/persistence';
import { SyncStore } from './sync/store';
import type { SyncTransport } from './sync/transport';
import { SyncTransportError } from './sync/transport';
import { ChangeKind, type SyncEntry, type SyncResponse } from './sync/wire';

const PROTOCOL = '2026-07-11';

function entry(overrides: Partial<SyncEntry> & { syncSeq: bigint }): SyncEntry {
    return {
        albumId: 'album-a',
        protocolVersion: PROTOCOL,
        kind: ChangeKind.Created,
        assetId: `asset-${overrides.syncSeq}`,
        manifestCbor: new Uint8Array(),
        metadataBlob: new Uint8Array(),
        originalHeld: true,
        ...overrides,
    };
}

/** A scripted transport: each call shifts the next page; empty page once drained. */
class ScriptedTransport implements SyncTransport {
    public calls = 0;
    constructor(private readonly pages: SyncResponse[]) {}
    sync(): Promise<SyncResponse> {
        this.calls += 1;
        const page = this.pages.shift();
        return Promise.resolve(
            page ?? { entries: [], nextCursor: new Uint8Array() },
        );
    }
}

/** A transport that always fails, simulating offline / no backend. */
class FailingTransport implements SyncTransport {
    sync(): Promise<SyncResponse> {
        return Promise.reject(
            new SyncTransportError(14, 'error.sync.unavailable', 'unavailable'),
        );
    }
}

function makeGateway(
    transport: SyncTransport,
    persistence = new MemoryPersistence(),
) {
    return {
        gateway: new ServerGateway({
            transport,
            store: new SyncStore(),
            persistence,
        }),
        persistence,
    };
}

describe('ServerGateway — key-free shells', () => {
    test('maps synced assets to display shells', async () => {
        const { gateway } = makeGateway(
            new ScriptedTransport([
                {
                    entries: [
                        entry({
                            syncSeq: 1n,
                            assetId: 'photo',
                            originalHeld: false,
                            blobs: {
                                original: {
                                    ciphertextHash: 'h',
                                    role: 'original',
                                    format: 'video/mp4',
                                    size: 10n,
                                },
                                derivatives: [],
                            },
                        }),
                    ],
                    nextCursor: new Uint8Array([1]),
                },
            ]),
        );

        const assets = await gateway.listAssets();
        expect(assets).toHaveLength(1);
        const asset = assets[0];
        expect(asset.id).toBe('photo');
        // Key-free: no renderable URL, no LQIP, placeholder dimensions.
        expect(asset.url).toBe('');
        expect(asset.width).toBe(1);
        expect(asset.height).toBe(1);
        // Real key-free facts: media kind from blob format, awaiting-original, protocol date.
        expect(asset.type).toBe('video');
        expect(asset.pending).toBe(true);
        expect(asset.date.getTime()).toBe(Date.parse('2026-07-11T00:00:00Z'));
    });

    test('exposes album membership counts and resolves a single album', async () => {
        const { gateway } = makeGateway(
            new ScriptedTransport([
                {
                    entries: [
                        entry({ syncSeq: 1n, albumId: 'a', assetId: 'a1' }),
                        entry({ syncSeq: 2n, albumId: 'a', assetId: 'a2' }),
                        entry({ syncSeq: 3n, albumId: 'b', assetId: 'b1' }),
                    ],
                    nextCursor: new Uint8Array([3]),
                },
            ]),
        );

        const albums = await gateway.listAlbums();
        expect(albums.map((x) => x.id).sort()).toEqual(['a', 'b']);
        const a = await gateway.getAlbum('a');
        expect(a?.assetCount).toBe(2);
        expect(a?.title).toBe(''); // display name is encrypted — absent key-free
        expect(await gateway.getAlbum('missing')).toBeNull();
        expect((await gateway.getAlbumAssets('a')).map((x) => x.id)).toEqual([
            'a2',
            'a1',
        ]);
    });
});

describe('ServerGateway — persistence', () => {
    test('a fresh gateway resumes from the persisted snapshot', async () => {
        const shared = new MemoryPersistence();
        const first = makeGateway(
            new ScriptedTransport([
                {
                    entries: [entry({ syncSeq: 1n, assetId: 'x' })],
                    nextCursor: new Uint8Array([1]),
                },
            ]),
            shared,
        );
        expect(await first.gateway.listAssets()).toHaveLength(1);

        // A second gateway over the same persistence, but a transport that returns nothing
        // new, must still see the asset from the restored snapshot.
        const second = makeGateway(new ScriptedTransport([]), shared);
        expect(await second.gateway.listAssets()).toHaveLength(1);
    });
});

describe('ServerGateway — resilience', () => {
    test('a transport failure serves the hydrated store without throwing', async () => {
        const { gateway } = makeGateway(new FailingTransport());
        // No backend reachable: queries resolve to empty rather than rejecting.
        expect(await gateway.listAssets()).toEqual([]);
        expect(await gateway.listAlbums()).toEqual([]);
    });

    test('bring-up runs once and is shared across concurrent reads', async () => {
        const transport = new ScriptedTransport([
            {
                entries: [entry({ syncSeq: 1n })],
                nextCursor: new Uint8Array([1]),
            },
        ]);
        const { gateway } = makeGateway(transport);
        await Promise.all([
            gateway.listAssets(),
            gateway.listAlbums(),
            gateway.getAlbum('album-a'),
        ]);
        // A single short page (< PAGE_SIZE) ends the catch-up in one call; bring-up is
        // shared across the concurrent reads rather than re-run per query.
        expect(transport.calls).toBe(1);
    });
});
