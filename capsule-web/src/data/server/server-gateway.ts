/**
 * The real `CapsuleGateway`: the web client's key-free read path (slice S-D6).
 *
 * It consumes the `capsule.sync.v1` feed over gRPC-web, validates-then-applies each page
 * into the client-side `SyncStore` (the browser's `library.sqlite` analogue), persists the
 * snapshot, and answers the gateway's read queries from that store. There is no server-side
 * query surface and the server decrypts nothing (api-surfaces design doc): the feed carries opaque manifests,
 * ciphertext metadata and blob content addresses, and the browser holds no album keys.
 *
 * Consequently the queries return KEY-FREE SHELLS. What is real and honest here: asset and
 * album ids, per-album membership and counts, the derived awaiting-original state, blob
 * content addresses, and change recency (`sync_seq`). What lives in encrypted metadata and
 * is therefore ABSENT: display titles, cover art, capture dates, pixel dimensions, LQIP
 * placeholders, locations, durations, and any renderable imagery (blobs are ciphertext with
 * no in-browser decrypt path yet — gateway.ts defers that wasm boundary). Those display
 * fields are filled with safe placeholders and documented in the S-D6 report; they light up
 * once a wasm decode/verify boundary lands beneath this gateway. S-G1 (retirement of the
 * legacy server-side query surface) required query PARITY on this real path — which the key-free
 * shells provide for the aggregate UI — not decrypted content; that surface is now removed.
 */

import type { Album, Asset } from '@/domain';
import { getAccessToken } from '@/lib/auth';
import type { CapsuleGateway } from '../gateway';
import { IndexedDbPersistence, type SyncPersistence } from './sync/persistence';
import {
    type AlbumSummary,
    type AssetRecord,
    CLIENT_MAX_PROTOCOL,
    SyncStore,
} from './sync/store';
import {
    RestSyncTransport,
    type SyncTransport,
    SyncTransportError,
} from './sync/transport';

/**
 * The API origin. The transport appends `/v1/sync`.
 *
 * It used to be the server *root* rather than a versioned prefix, because gRPC addresses a
 * method by its fully-qualified path and native tonic clients discard any path on the endpoint
 * URI — so a prefixed mount was unreachable from them, and versioning rode the proto package
 * (`capsule.sync.v1`) instead of the URL. With the feed on REST (`S-C2`) the version is back in
 * the path, where a person reading a request log can see it.
 */
const API_BASE = import.meta.env.PUBLIC_API_URL ?? 'http://localhost:3000';
/** Page size requested from the feed (the server clamps to its own max). */
const PAGE_SIZE = 256;
/** Bound the initial catch-up so a hostile/huge feed cannot spin forever in one pass. */
const MAX_PAGES_PER_SYNC = 1000;

/** Everything the gateway composes; injectable so it is testable without a live server. */
export interface ServerGatewayDeps {
    transport: SyncTransport;
    store: SyncStore;
    persistence: SyncPersistence;
}

export class ServerGateway implements CapsuleGateway {
    private readonly transport: SyncTransport;
    private readonly store: SyncStore;
    private readonly persistence: SyncPersistence;
    /** Cached hydrate + first-sync pass, so concurrent reads share one bring-up. */
    private ready: Promise<void> | null = null;

    constructor(deps: ServerGatewayDeps) {
        this.transport = deps.transport;
        this.store = deps.store;
        this.persistence = deps.persistence;
    }

    async listAssets(): Promise<Asset[]> {
        await this.ensureReady();
        return this.store.listAssets().map(toAsset);
    }

    async listAlbums(): Promise<Album[]> {
        await this.ensureReady();
        return this.store.albums().map(toAlbum);
    }

    async getAlbum(id: string): Promise<Album | null> {
        await this.ensureReady();
        const summary = this.store.getAlbum(id);
        return summary ? toAlbum(summary) : null;
    }

    async getAlbumAssets(albumId: string): Promise<Asset[]> {
        await this.ensureReady();
        return this.store.assetsForAlbum(albumId).map(toAsset);
    }

    /**
     * Force a fresh feed pull (e.g. a user-driven refresh). Safe to call repeatedly; the
     * store's anti-rewind guard rejects a hostile rewind rather than corrupting state.
     */
    async refresh(): Promise<void> {
        await this.ensureReady();
        await this.sync();
    }

    /** Hydrate from persistence once, then run a best-effort first sync pass. */
    private ensureReady(): Promise<void> {
        if (!this.ready) {
            this.ready = this.bringUp();
        }
        return this.ready;
    }

    private async bringUp(): Promise<void> {
        try {
            const snapshot = await this.persistence.load();
            if (snapshot) {
                this.store.restore(snapshot);
            }
        } catch (err) {
            console.warn('sync store: failed to load persisted snapshot', err);
        }
        // A first sync failure (offline / no backend) must not blank the UI — the queries
        // still serve whatever was hydrated (possibly empty). Surfacing is the UI's job.
        await this.sync();
    }

    /** Pull feed pages from the persisted cursor until caught up, applying + persisting. */
    private async sync(): Promise<void> {
        try {
            for (let page = 0; page < MAX_PAGES_PER_SYNC; page++) {
                const response = await this.transport.sync({
                    cursor: this.store.cursor,
                    pageSize: PAGE_SIZE,
                });
                if (response.entries.length === 0) {
                    break;
                }
                // Throws on rewind / forward-version / structural — leaves the store intact.
                this.store.applyPage(response.entries, response.nextCursor);
                await this.persistence.save(this.store.snapshot());
                if (response.entries.length < PAGE_SIZE) {
                    break;
                }
            }
        } catch (err) {
            if (err instanceof SyncTransportError) {
                console.warn(
                    `sync feed unavailable (HTTP ${err.httpStatus}${err.errorCode ? ` / ${err.errorCode}` : ''}): ${err.message}`,
                );
                return;
            }
            // Validation failures (rewind / forward-version / structural) are surfaced: they
            // indicate a malicious or buggy server, per the download-sync client rules.
            console.error('sync feed rejected a page', err);
        }
    }
}

/** Map a key-free asset record to the UI's display shape (placeholders where encrypted). */
function toAsset(record: AssetRecord): Asset {
    return {
        id: record.assetId,
        // Blobs are ciphertext with no in-browser decrypt path yet — no renderable URL.
        url: '',
        thumbnailUrl: '',
        // The only envelope-visible date is the album protocol pin (NOT capture time).
        date: protocolDate(record.protocolVersion),
        type: deriveType(record),
        // duration / location live in encrypted metadata — absent key-free.
        location: undefined,
        duration: undefined,
        // Pixel dimensions live in encrypted metadata; 1×1 keeps justified layout finite.
        width: 1,
        height: 1,
        pending: record.originalHeld ? undefined : true,
    };
}

/** Map a key-free album summary to the UI's display shape. */
function toAlbum(summary: AlbumSummary): Album {
    return {
        id: summary.albumId,
        // Display title lives in encrypted album metadata — absent key-free.
        title: '',
        // Cover art is a ciphertext blob — no renderable URL key-free.
        coverUrl: '',
        // The one honest, key-free album fact: live membership count.
        assetCount: summary.assetCount,
    };
}

/**
 * The media kind, which a key-free client cannot know.
 *
 * This used to guess from the gRPC feed's per-blob `format` string. The REST feed does not carry
 * one, and its absence is the point rather than an omission: a MIME type is plaintext media
 * metadata about an encrypted blob, and a key-free server holding it is exactly what the upload
 * envelope contract forbids. So the guess is gone with the field it read.
 *
 * Everything therefore renders as the neutral shell until a key-holding client decrypts the
 * metadata blob and says otherwise. A wrong badge on a video is a worse answer than no badge.
 */
function deriveType(_record: AssetRecord): Asset['type'] {
    return 'image';
}

/** Parse an album protocol pin (`YYYY-MM-DD`) to a UTC date; epoch on a malformed pin. */
function protocolDate(protocolVersion: string): Date {
    const date = new Date(`${protocolVersion}T00:00:00Z`);
    return Number.isNaN(date.getTime()) ? new Date(0) : date;
}

/**
 * Build the browser-backed gateway: REST transport → in-memory store → IndexedDB
 * persistence, with the bearer token pulled from the auth token store. Called from
 * `../index.ts`; all DOM/network access is lazy, so import + construction are side-effect
 * free.
 */
export function createBrowserServerGateway(): ServerGateway {
    return new ServerGateway({
        transport: new RestSyncTransport({
            baseUrl: API_BASE,
            protocol: CLIENT_MAX_PROTOCOL,
            accessToken: getAccessToken,
        }),
        store: new SyncStore(),
        persistence: new IndexedDbPersistence(),
    });
}
