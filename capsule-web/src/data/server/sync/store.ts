/**
 * The client-side sync store — the browser's key-free analogue of `library.sqlite`.
 *
 * It consumes the sync feed and answers the gateway's read queries entirely client-side
 * (api-surfaces: "Library queries … none — client-side"). It is KEY-FREE: the feed's
 * signed manifest is opaque CBOR and its metadata blob is ciphertext, and the browser
 * holds no album keys, so the store retains only what the feed's envelope legitimately
 * exposes — ids, per-album `sync_seq`, the album protocol pin, blob content addresses by
 * role, and the derived `original_held` state. Capture dates, dimensions, titles, LQIP
 * and locations live in the encrypted metadata and are NOT available here (see the S-D6
 * report); the gateway maps these records to key-free display shells.
 *
 * Validation mirrors the S-D2 client rules (download-sync design doc, Validation):
 *   - Forward-version rejection: an entry whose `protocol_version` is above the client's
 *     max known is refused WITHOUT partial application.
 *   - Anti-rewind: a page whose `sync_seq` regresses against the per-album high-water mark
 *     is surfaced, not applied.
 * A page is validated in full before any mutation, so a rejected page leaves the store
 * untouched. The opaque cursor and the high-water marks persist together in one snapshot,
 * so they can never diverge across a crash (an unpersisted apply simply refetches the same
 * page from the old cursor and re-applies cleanly).
 */

import { ChangeKind, type SyncEntry } from './wire';

/**
 * The newest album protocol pin (`YYYY-MM-DD`) this client understands — both the version
 * it advertises in `x-capsule-protocol` and the ceiling for feed-entry forward-version
 * rejection. Mirrors the server's `DEFAULT_PROTOCOL_MAX`. ISO dates compare correctly as
 * strings, so the check is a lexicographic `>`.
 */
export const CLIENT_MAX_PROTOCOL = '2026-12-31';

/** A feed entry carried a `protocol_version` above the client's max known. */
export class SyncProtocolError extends Error {
    constructor(
        public readonly protocolVersion: string,
        public readonly assetId: string,
    ) {
        super(
            `feed entry protocol_version ${protocolVersion} exceeds client max ${CLIENT_MAX_PROTOCOL} (asset ${assetId})`,
        );
        this.name = 'SyncProtocolError';
    }
}

/** A page's `sync_seq` regressed against the locally-seen high-water mark. */
export class SyncRewindError extends Error {
    constructor(
        public readonly albumId: string,
        public readonly seen: bigint,
        public readonly got: bigint,
    ) {
        super(
            `sync_seq rewind for album ${albumId}: high-water ${seen}, got ${got}`,
        );
        this.name = 'SyncRewindError';
    }
}

/** A blob content address by role, as the feed exposes it (never blob bytes). */
export interface StoredBlobRef {
    hash: string;
    role: string;
    size: bigint;
}

/** The key-free facts the store keeps per asset. */
export interface AssetRecord {
    assetId: string;
    albumId: string;
    /** Per-album sequence at which this asset last changed (recency proxy). */
    syncSeq: bigint;
    /** The album protocol pin (`YYYY-MM-DD`) — the only envelope-visible date. */
    protocolVersion: string;
    /** Whether the original blob is finalized server-side (false ⇒ awaiting-original). */
    originalHeld: boolean;
    original?: StoredBlobRef;
    derivatives: StoredBlobRef[];
}

/** A key-free album summary derived from live asset membership. */
export interface AlbumSummary {
    albumId: string;
    /** Count of live (non-tombstoned) assets — a real, key-free fact. */
    assetCount: number;
    /** Highest `sync_seq` among the album's assets (recency proxy for ordering). */
    latestSeq: bigint;
    /** The album protocol pin from its most recent entry. */
    protocolVersion: string;
}

interface PersistedBlobRef {
    hash: string;
    role: string;
    format: string;
    size: string;
}

interface PersistedAsset {
    assetId: string;
    albumId: string;
    syncSeq: string;
    protocolVersion: string;
    originalHeld: boolean;
    original?: PersistedBlobRef;
    derivatives: PersistedBlobRef[];
}

/** The full serializable state — cursor and high-water persist atomically together. */
export interface SyncSnapshot {
    version: 1;
    /**
     * The opaque server cursor; empty string before the first sync.
     *
     * It used to be base64 of the gRPC feed's cursor *bytes*. The REST feed's cursor is already
     * a URL-safe string, so there is nothing left to encode — and one fewer encoding is one
     * fewer place a resumption token can be mangled in transit through storage.
     */
    cursor: string;
    /** Per-album high-water marks (`albumId` → decimal `sync_seq`). */
    highWater: Record<string, string>;
    assets: PersistedAsset[];
}

function toStoredRef(ref: {
    ciphertextHash: string;
    role: string;
    size: bigint;
}): StoredBlobRef {
    return {
        hash: ref.ciphertextHash,
        role: ref.role,
        size: ref.size,
    };
}

/**
 * The in-memory, key-free sync store. Pure logic (no DOM, no persistence) so it is
 * unit-testable directly; `ServerGateway` composes it with a transport and a persistence
 * adapter.
 */
export class SyncStore {
    private assets = new Map<string, AssetRecord>();
    private highWater = new Map<string, bigint>();
    private cursorToken = '';

    /** The opaque resumption cursor for the next feed request. */
    get cursor(): string {
        return this.cursorToken;
    }

    /**
     * Validate and apply a feed page, then advance the cursor. Throws
     * `SyncProtocolError` / `SyncRewindError` WITHOUT mutating the store or cursor when the
     * page is invalid. A structurally impossible entry never gets here — see below.
     */
    applyPage(entries: SyncEntry[], nextCursor: string): void {
        // Pass 1 — validate the whole page against a scratch copy of the high-water marks.
        //
        // There is no `ChangeKind.Unspecified` check here any more, and the guarantee it gave
        // has not gone: it existed because a proto3 enum defaults to 0 when the field is absent,
        // so an unset kind arrived here looking like a value. On the REST feed the kind is a
        // closed string enum and an unknown one is a `SyncDecodeError` at the transport
        // boundary, which is strictly earlier — the page never reaches the store at all.
        const advanced = new Map(this.highWater);
        for (const entry of entries) {
            if (entry.protocolVersion > CLIENT_MAX_PROTOCOL) {
                throw new SyncProtocolError(
                    entry.protocolVersion,
                    entry.assetId,
                );
            }
            const seen = advanced.get(entry.albumId) ?? 0n;
            if (entry.syncSeq <= seen) {
                throw new SyncRewindError(entry.albumId, seen, entry.syncSeq);
            }
            advanced.set(entry.albumId, entry.syncSeq);
        }

        // Pass 2 — apply atomically; the page is known valid.
        for (const entry of entries) {
            if (entry.kind === ChangeKind.Deleted) {
                this.assets.delete(entry.assetId);
                continue;
            }
            this.assets.set(entry.assetId, {
                assetId: entry.assetId,
                albumId: entry.albumId,
                syncSeq: entry.syncSeq,
                protocolVersion: entry.protocolVersion,
                originalHeld: entry.originalHeld,
                original: entry.blobs?.original
                    ? toStoredRef(entry.blobs.original)
                    : undefined,
                derivatives: (entry.blobs?.derivatives ?? []).map(toStoredRef),
            });
        }
        this.highWater = advanced;
        this.cursorToken = nextCursor;
    }

    /** All live assets, newest change first (by `sync_seq` desc, id as a stable tiebreak). */
    listAssets(): AssetRecord[] {
        return [...this.assets.values()].sort((a, b) => {
            if (a.syncSeq === b.syncSeq) {
                return a.assetId < b.assetId ? 1 : -1;
            }
            return a.syncSeq > b.syncSeq ? -1 : 1;
        });
    }

    /** Live assets belonging to `albumId`, newest change first. */
    assetsForAlbum(albumId: string): AssetRecord[] {
        return this.listAssets().filter((a) => a.albumId === albumId);
    }

    /** Key-free album summaries, most-recently-active first. */
    albums(): AlbumSummary[] {
        const byAlbum = new Map<string, AlbumSummary>();
        for (const asset of this.assets.values()) {
            const existing = byAlbum.get(asset.albumId);
            if (existing) {
                existing.assetCount += 1;
                if (asset.syncSeq > existing.latestSeq) {
                    existing.latestSeq = asset.syncSeq;
                    existing.protocolVersion = asset.protocolVersion;
                }
            } else {
                byAlbum.set(asset.albumId, {
                    albumId: asset.albumId,
                    assetCount: 1,
                    latestSeq: asset.syncSeq,
                    protocolVersion: asset.protocolVersion,
                });
            }
        }
        return [...byAlbum.values()].sort((a, b) =>
            a.latestSeq > b.latestSeq ? -1 : a.latestSeq < b.latestSeq ? 1 : 0,
        );
    }

    /** A single album summary, or null when the album has no live assets. */
    getAlbum(albumId: string): AlbumSummary | null {
        return this.albums().find((a) => a.albumId === albumId) ?? null;
    }

    /** Serialize the full state for persistence (cursor + high-water + records). */
    snapshot(): SyncSnapshot {
        const highWater: Record<string, string> = {};
        for (const [albumId, seq] of this.highWater) {
            highWater[albumId] = seq.toString();
        }
        return {
            version: 1,
            cursor: this.cursorToken,
            highWater,
            assets: [...this.assets.values()].map((a) => ({
                assetId: a.assetId,
                albumId: a.albumId,
                syncSeq: a.syncSeq.toString(),
                protocolVersion: a.protocolVersion,
                originalHeld: a.originalHeld,
                original: a.original ? persistRef(a.original) : undefined,
                derivatives: a.derivatives.map(persistRef),
            })),
        };
    }

    /** Rehydrate from a snapshot, replacing all state. */
    restore(snapshot: SyncSnapshot): void {
        this.assets = new Map(
            snapshot.assets.map((a) => [
                a.assetId,
                {
                    assetId: a.assetId,
                    albumId: a.albumId,
                    syncSeq: BigInt(a.syncSeq),
                    protocolVersion: a.protocolVersion,
                    originalHeld: a.originalHeld,
                    original: a.original ? restoreRef(a.original) : undefined,
                    derivatives: a.derivatives.map(restoreRef),
                },
            ]),
        );
        this.highWater = new Map(
            Object.entries(snapshot.highWater).map(([k, v]) => [k, BigInt(v)]),
        );
        this.cursorToken = snapshot.cursor;
    }
}

function persistRef(ref: StoredBlobRef): PersistedBlobRef {
    return {
        hash: ref.hash,
        role: ref.role,
        format: ref.format,
        size: ref.size.toString(),
    };
}

function restoreRef(ref: PersistedBlobRef): StoredBlobRef {
    return {
        hash: ref.hash,
        role: ref.role,
        format: ref.format,
        size: BigInt(ref.size),
    };
}
