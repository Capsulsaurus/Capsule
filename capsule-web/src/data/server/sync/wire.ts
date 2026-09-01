/**
 * Wire types for `GET /v1/sync` — the Kynos REST sync feed.
 *
 * This module used to be a hand-rolled Protobuf + gRPC-web codec: 410 lines of varint and
 * length-delimited framing, written because browsers cannot speak native gRPC and pulling a
 * `protobuf-es`/`connect-web` toolchain into the bun build to read a handful of fields was not
 * worth it. The feed is REST/JSON now (`S-C2`, `S-D28`) and the whole codec is gone — the
 * server's own OpenAPI document is the contract, `JSON.parse` is the parser, and the framing
 * problem does not exist.
 *
 * What is kept is the **shape** these types present to `store.ts`, so the store's validation
 * rules — forward-version rejection and per-album anti-rewind — are unchanged by the transport
 * swap. That was the point of the seam.
 *
 * INVARIANT — the signed `AssetManifest` (`manifestCbor`) and the encrypted metadata blob travel
 * as OPAQUE values and are never decoded here: the browser holds no album keys (`gateway.ts`).
 */

/** What changed for an asset, as `WireChangeKind` spells it on the wire. */
export enum ChangeKind {
    Created = 'created',
    Updated = 'updated',
    Deleted = 'deleted',
}

/** A blob's role in its asset bundle, as `WireBlobRole` spells it on the wire. */
export enum BlobRole {
    Original = 'original',
    Derivative = 'derivative',
    Metadata = 'metadata',
    Provenance = 'provenance',
    Backup = 'backup',
}

/** A content-addressed blob reference (never blob bytes). */
export interface BlobRef {
    /** Ciphertext content address, lowercase hex. */
    ciphertextHash: string;
    /** The blob's role in the bundle. */
    role: BlobRole;
    /** Ciphertext size in bytes, so a client can budget a fetch before issuing one. */
    size: bigint;
}

/** Per-role content addresses of an asset's blobs. */
export interface BlobManifest {
    original?: BlobRef;
    derivatives: BlobRef[];
}

/** One change in the feed. */
export interface SyncEntry {
    /** The asset that changed. */
    assetId: string;
    /** The album it belongs to; the store keeps its anti-rewind high-water mark per album. */
    albumId: string;
    /** The album's pinned protocol date (`YYYY-MM-DD`). */
    protocolVersion: string;
    /** The entry's position. Strictly increasing within a page. */
    syncSeq: bigint;
    /** What this is, relative to the client that asked. */
    kind: ChangeKind;
    /**
     * The signed manifest — base64 of the provenance blob's exact bytes. Opaque here.
     *
     * Absent on a tombstone, and absent when the index names a provenance blob the store
     * cannot produce (the server logs loudly in that case).
     */
    manifestCbor?: string;
    /** The encrypted metadata blob's content address. Opaque here. */
    metadataBlob?: string;
    /** The asset's blobs, by role. */
    blobs: BlobManifest;
    /** Whether the original has landed. */
    originalHeld: boolean;
    /** When the change happened, RFC 3339. */
    changedAt: string;
}

/** A page of the feed. */
export interface SyncResponse {
    entries: SyncEntry[];
    /**
     * The cursor that resumes after the last entry.
     *
     * Always present, including on an empty page, where the server re-mints the position the
     * client arrived with — so a client never has to decide whether to keep its old cursor.
     */
    nextCursor: string;
    /** Whether the server holds changes beyond this page. */
    hasMore: boolean;
}

/** A request for one page. */
export interface SyncRequest {
    /** The opaque cursor a previous page returned; absent means "from the beginning". */
    cursor?: string;
    /** How many entries to ask for. The server clamps it into the range it serves. */
    pageSize?: number;
}

/** The document's `SyncBlobRef`, before it is grouped by role. */
interface WireBlobRef {
    role: string;
    hash: string;
    size: number;
}

/** The document's `SyncEntry`. */
interface WireSyncEntry {
    asset_id: string;
    album_id: string;
    protocol_version: string;
    sync_seq: number;
    change: string;
    manifest_cbor?: string | null;
    metadata_blob?: string | null;
    blobs: WireBlobRef[];
    original_held: boolean;
    changed_at: string;
}

/** The document's `SyncPageResponse`. */
interface WireSyncPage {
    entries: WireSyncEntry[];
    next_cursor: string;
    has_more: boolean;
}

/** A page body that is not the shape the contract promises. */
export class SyncDecodeError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'SyncDecodeError';
    }
}

/** Read a wire role, refusing one the closed enum does not name. */
function decodeRole(role: string): BlobRole {
    switch (role) {
        case 'original':
            return BlobRole.Original;
        case 'derivative':
            return BlobRole.Derivative;
        case 'metadata':
            return BlobRole.Metadata;
        case 'provenance':
            return BlobRole.Provenance;
        case 'backup':
            return BlobRole.Backup;
        default:
            throw new SyncDecodeError(
                `unknown blob role ${JSON.stringify(role)}`,
            );
    }
}

/** Read a wire change kind, refusing one the closed enum does not name. */
function decodeChange(change: string): ChangeKind {
    switch (change) {
        case 'created':
            return ChangeKind.Created;
        case 'updated':
            return ChangeKind.Updated;
        case 'deleted':
            return ChangeKind.Deleted;
        default:
            throw new SyncDecodeError(
                `unknown change kind ${JSON.stringify(change)}`,
            );
    }
}

/**
 * Group an entry's blob list by role.
 *
 * The REST feed serves one flat list; the store wants the original separately from the
 * derivatives, because "is the original held" is the state it renders. Metadata, provenance and
 * backup blobs are addressed elsewhere in the entry or are not the browser's business, so they
 * are dropped here rather than carried into a shape that has nowhere to put them.
 */
function groupBlobs(blobs: WireBlobRef[]): BlobManifest {
    const manifest: BlobManifest = { derivatives: [] };
    for (const blob of blobs) {
        const ref: BlobRef = {
            ciphertextHash: blob.hash,
            role: decodeRole(blob.role),
            size: BigInt(blob.size),
        };
        if (ref.role === BlobRole.Original) {
            manifest.original = ref;
        } else if (ref.role === BlobRole.Derivative) {
            manifest.derivatives.push(ref);
        }
    }
    return manifest;
}

/**
 * Decode one page body.
 *
 * Structural rather than trusting: a body missing a required field is a `SyncDecodeError` and
 * not `undefined` propagating into the store, because the store's anti-rewind check compares
 * sequence numbers and `undefined` compares false against everything.
 */
export function decodeSyncResponse(body: unknown): SyncResponse {
    const page = body as WireSyncPage;
    if (
        !page ||
        !Array.isArray(page.entries) ||
        typeof page.next_cursor !== 'string'
    ) {
        throw new SyncDecodeError(
            'sync page is missing `entries` or `next_cursor`',
        );
    }

    return {
        entries: page.entries.map((entry) => {
            if (
                typeof entry.asset_id !== 'string' ||
                typeof entry.album_id !== 'string' ||
                typeof entry.protocol_version !== 'string' ||
                typeof entry.sync_seq !== 'number' ||
                typeof entry.changed_at !== 'string'
            ) {
                throw new SyncDecodeError(
                    `sync entry is missing a required field: ${JSON.stringify(entry)}`,
                );
            }
            return {
                assetId: entry.asset_id,
                albumId: entry.album_id,
                protocolVersion: entry.protocol_version,
                syncSeq: BigInt(entry.sync_seq),
                kind: decodeChange(entry.change),
                manifestCbor: entry.manifest_cbor ?? undefined,
                metadataBlob: entry.metadata_blob ?? undefined,
                blobs: groupBlobs(entry.blobs ?? []),
                originalHeld: entry.original_held === true,
                changedAt: entry.changed_at,
            };
        }),
        nextCursor: page.next_cursor,
        hasMore: page.has_more === true,
    };
}

/** The query string for one page request. */
export function encodeSyncRequest(request: SyncRequest): string {
    const params = new URLSearchParams();
    if (request.cursor !== undefined) {
        params.set('cursor', request.cursor);
    }
    if (request.pageSize !== undefined) {
        params.set('page_size', String(request.pageSize));
    }
    const query = params.toString();
    return query.length > 0 ? `?${query}` : '';
}
