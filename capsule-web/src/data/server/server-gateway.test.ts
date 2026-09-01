import { describe, expect, test } from 'bun:test';

import { ServerGateway } from './server-gateway';
import { MemoryPersistence } from './sync/persistence';
import { SyncStore } from './sync/store';
import type { SyncTransport } from './sync/transport';
import { SyncTransportError } from './sync/transport';
import {
    BlobRole,
    ChangeKind,
    type SyncEntry,
    type SyncResponse,
} from './sync/wire';

const PROTOCOL = '2026-07-11';

function entry(overrides: Partial<SyncEntry> & { syncSeq: bigint }): SyncEntry {
    return {
        albumId: 'album-a',
        protocolVersion: PROTOCOL,
        kind: ChangeKind.Created,
        assetId: `asset-${overrides.syncSeq}`,
        manifestCbor: 'AQID',
        metadataBlob: 'a'.repeat(64),
        blobs: { derivatives: [] },
        originalHeld: true,
        changedAt: '2026-09-01T00:00:00Z',
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
            page ?? { entries: [], nextCursor: '', hasMore: false },
        );
    }
}

/** A transport that always fails, simulating offline / no backend. */
class FailingTransport implements SyncTransport {
    sync(): Promise<SyncResponse> {
        return Promise.reject(
            new SyncTransportError(
                503,
                'error.sync.unavailable',
                'unavailable',
            ),
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
                                    role: BlobRole.Original,
                                    size: 10n,
                                },
                                derivatives: [],
                            },
                        }),
                    ],
                    nextCursor: 'cursor-1',
                    hasMore: false,
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
        // The media kind is *not* a key-free fact any more, and that is the REST feed being
        // stricter than the gRPC one: a MIME type is plaintext metadata about an encrypted
        // blob, so the field the old guess read is gone and everything is the neutral shell.
        expect(asset.type).toBe('image');
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
                    nextCursor: 'cursor-3',
                    hasMore: false,
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
                    nextCursor: 'cursor-1',
                    hasMore: false,
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
                nextCursor: 'cursor-1',
                hasMore: false,
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
