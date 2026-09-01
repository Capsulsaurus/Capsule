/**
 * Wire codec for the `capsule.sync.v1.SyncService` feed — proto-mirror types plus a
 * minimal Protobuf + gRPC-web framing implementation.
 *
 * The feed is gRPC (tonic behind the salvo bridge, api-surfaces design doc). Browsers
 * cannot speak native gRPC, so the web client speaks **gRPC-web** — the sanctioned
 * browser carriage of the same proto contract — against `POST
 * {base}/capsule.sync.v1.SyncService/Sync` with `content-type
 * application/grpc-web+proto`.
 *
 * The message set is small and frozen (a handful of fields), so we hand-roll the exact
 * subset of Protobuf wire types the feed uses (varint + length-delimited) rather than
 * pulling a full `protobuf-es`/`connect-web` toolchain and its codegen into the bun
 * build. The codec is pure (`Uint8Array` in/out) and golden-tested in `wire.test.ts`; if
 * the proto grows past this subset, swap this module for generated code behind the same
 * types (see the S-D6 report).
 *
 * INVARIANT — the signed `AssetManifest` (`manifestCbor`) and the encrypted
 * `metadataBlob` travel as OPAQUE bytes and are never decoded here: the browser holds no
 * album keys (gateway.ts). The server serializes the id / hash "bytes" fields as the
 * UTF-8 bytes of their string form (see the server's `routes::sync`), so this
 * codec surfaces them as strings — matching what is actually on the wire.
 */

/** What changed for an asset. Closed enum, mirrors `capsule.sync.v1.ChangeKind`. */
export enum ChangeKind {
    Unspecified = 0,
    Created = 1,
    MetadataUpdated = 2,
    Deleted = 3,
}

/** A content-addressed blob reference (never blob bytes). */
export interface BlobRef {
    /** Ciphertext content address (server sends the hash string's UTF-8 bytes). */
    ciphertextHash: string;
    /** `original | metadata | derivative | provenance`. */
    role: string;
    /** MIME/format string, for derivatives. */
    format: string;
    /** Ciphertext size in bytes. */
    size: bigint;
}

/** Per-role content addresses of an asset's blobs. */
export interface BlobManifest {
    original?: BlobRef;
    derivatives: BlobRef[];
}

/** One feed entry — a single change to a single asset. */
export interface SyncEntry {
    /** Album this entry belongs to (UUID string). */
    albumId: string;
    /** Per-album strictly-increasing sequence — the anti-rewind high-water mark. */
    syncSeq: bigint;
    /** Album protocol pin this entry conforms to (`YYYY-MM-DD`). */
    protocolVersion: string;
    /** What changed. */
    kind: ChangeKind;
    /** Asset id (UUID string). */
    assetId: string;
    /** Signed `AssetManifest` as opaque canonical CBOR — never decoded key-free. */
    manifestCbor: Uint8Array;
    /** Encrypted metadata blob — empty for deletes; never decoded key-free. */
    metadataBlob: Uint8Array;
    /** Per-role blob content addresses. */
    blobs?: BlobManifest;
    /** Whether the original blob is finalized server-side (staged uploads). */
    originalHeld: boolean;
}

/** A `Sync` request: an opaque resumption cursor and a page-size hint. */
export interface SyncRequest {
    cursor: Uint8Array;
    pageSize: number;
}

/** A `Sync` response page. */
export interface SyncResponse {
    entries: SyncEntry[];
    nextCursor: Uint8Array;
}

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

// ── Protobuf wire types we use ───────────────────────────────────────────────
const WIRE_VARINT = 0;
const WIRE_LEN = 2;

/** A malformed wire buffer (framing or Protobuf). */
export class WireError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'WireError';
    }
}

/** A cursor over a Protobuf message body. */
class Reader {
    private pos = 0;

    constructor(private readonly buf: Uint8Array) {}

    eof(): boolean {
        return this.pos >= this.buf.length;
    }

    /** Read a base-128 varint as a `bigint` (covers `u64` without precision loss). */
    varint(): bigint {
        let result = 0n;
        let shift = 0n;
        for (;;) {
            if (this.pos >= this.buf.length) {
                throw new WireError('truncated varint');
            }
            const byte = this.buf[this.pos++];
            result |= BigInt(byte & 0x7f) << shift;
            if ((byte & 0x80) === 0) {
                break;
            }
            shift += 7n;
        }
        return result;
    }

    /** A field tag: `(fieldNumber << 3) | wireType`. */
    tag(): { field: number; wire: number } {
        const tag = Number(this.varint());
        return { field: tag >>> 3, wire: tag & 0x7 };
    }

    /** A length-delimited byte slice (bytes / string / embedded message). */
    bytes(): Uint8Array {
        const len = Number(this.varint());
        if (this.pos + len > this.buf.length) {
            throw new WireError('truncated length-delimited field');
        }
        const slice = this.buf.subarray(this.pos, this.pos + len);
        this.pos += len;
        return slice;
    }

    string(): string {
        return textDecoder.decode(this.bytes());
    }

    /** Skip a field of unknown number so forward-compatible fields don't break decode. */
    skip(wire: number): void {
        switch (wire) {
            case WIRE_VARINT:
                this.varint();
                break;
            case WIRE_LEN:
                this.bytes();
                break;
            case 5: // fixed32
                this.pos += 4;
                break;
            case 1: // fixed64
                this.pos += 8;
                break;
            default:
                throw new WireError(`unsupported wire type ${wire}`);
        }
    }
}

/** An append-only Protobuf message writer. */
class Writer {
    private readonly parts: number[] = [];

    private varint(value: bigint): void {
        let v = value;
        for (;;) {
            const byte = Number(v & 0x7fn);
            v >>= 7n;
            if (v === 0n) {
                this.parts.push(byte);
                break;
            }
            this.parts.push(byte | 0x80);
        }
    }

    tag(field: number, wire: number): void {
        this.varint(BigInt((field << 3) | wire));
    }

    varintField(field: number, value: number | bigint): void {
        this.tag(field, WIRE_VARINT);
        this.varint(BigInt(value));
    }

    bytesField(field: number, value: Uint8Array): void {
        this.tag(field, WIRE_LEN);
        this.varint(BigInt(value.length));
        for (const b of value) {
            this.parts.push(b);
        }
    }

    finish(): Uint8Array {
        return Uint8Array.from(this.parts);
    }
}

// ── Message decoders ─────────────────────────────────────────────────────────

function decodeBlobRef(buf: Uint8Array): BlobRef {
    const r = new Reader(buf);
    const ref: BlobRef = {
        ciphertextHash: '',
        role: '',
        format: '',
        size: 0n,
    };
    while (!r.eof()) {
        const { field, wire } = r.tag();
        switch (field) {
            case 1:
                ref.ciphertextHash = r.string();
                break;
            case 2:
                ref.role = r.string();
                break;
            case 3:
                ref.format = r.string();
                break;
            case 4:
                ref.size = r.varint();
                break;
            default:
                r.skip(wire);
        }
    }
    return ref;
}

function decodeBlobManifest(buf: Uint8Array): BlobManifest {
    const r = new Reader(buf);
    const manifest: BlobManifest = { derivatives: [] };
    while (!r.eof()) {
        const { field, wire } = r.tag();
        switch (field) {
            case 1:
                manifest.original = decodeBlobRef(r.bytes());
                break;
            case 2:
                manifest.derivatives.push(decodeBlobRef(r.bytes()));
                break;
            default:
                r.skip(wire);
        }
    }
    return manifest;
}

function decodeSyncEntry(buf: Uint8Array): SyncEntry {
    const r = new Reader(buf);
    const entry: SyncEntry = {
        albumId: '',
        syncSeq: 0n,
        protocolVersion: '',
        kind: ChangeKind.Unspecified,
        assetId: '',
        manifestCbor: new Uint8Array(),
        metadataBlob: new Uint8Array(),
        originalHeld: false,
    };
    while (!r.eof()) {
        const { field, wire } = r.tag();
        switch (field) {
            case 1:
                entry.albumId = r.string();
                break;
            case 2:
                entry.syncSeq = r.varint();
                break;
            case 3:
                entry.protocolVersion = r.string();
                break;
            case 4:
                entry.kind = Number(r.varint()) as ChangeKind;
                break;
            case 5:
                entry.assetId = r.string();
                break;
            case 6:
                entry.manifestCbor = r.bytes().slice();
                break;
            case 7:
                entry.metadataBlob = r.bytes().slice();
                break;
            case 8:
                entry.blobs = decodeBlobManifest(r.bytes());
                break;
            case 9:
                entry.originalHeld = r.varint() !== 0n;
                break;
            default:
                r.skip(wire);
        }
    }
    return entry;
}

/** Decode a `SyncResponse` Protobuf message body. */
export function decodeSyncResponse(buf: Uint8Array): SyncResponse {
    const r = new Reader(buf);
    const response: SyncResponse = {
        entries: [],
        nextCursor: new Uint8Array(),
    };
    while (!r.eof()) {
        const { field, wire } = r.tag();
        switch (field) {
            case 1:
                response.entries.push(decodeSyncEntry(r.bytes()));
                break;
            case 2:
                response.nextCursor = r.bytes().slice();
                break;
            default:
                r.skip(wire);
        }
    }
    return response;
}

/** Encode a `SyncRequest` Protobuf message body (proto3 omits default-valued fields). */
export function encodeSyncRequest(req: SyncRequest): Uint8Array {
    const w = new Writer();
    if (req.cursor.length > 0) {
        w.bytesField(1, req.cursor);
    }
    if (req.pageSize > 0) {
        w.varintField(2, req.pageSize);
    }
    return w.finish();
}

// ── gRPC-web framing ─────────────────────────────────────────────────────────
// Length-Prefixed-Message: 1 flag byte (0x00 data, 0x80 trailer) + 4-byte big-endian
// length + payload. Trailers ride the response body as an HTTP/1.1-style block.

const FLAG_TRAILER = 0x80;

/** Frame a single Protobuf message for a gRPC-web request body. */
export function frameRequest(message: Uint8Array): Uint8Array {
    const out = new Uint8Array(5 + message.length);
    out[0] = 0x00;
    new DataView(out.buffer).setUint32(1, message.length, false);
    out.set(message, 5);
    return out;
}

/** A decoded gRPC-web response body: data messages plus the parsed trailer block. */
export interface GrpcWebFrames {
    messages: Uint8Array[];
    trailers: Map<string, string>;
}

/** Split a gRPC-web response body into its data messages and trailer metadata. */
export function parseResponseFrames(body: Uint8Array): GrpcWebFrames {
    const messages: Uint8Array[] = [];
    let trailers = new Map<string, string>();
    const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
    let pos = 0;
    while (pos + 5 <= body.length) {
        const flag = body[pos];
        const len = view.getUint32(pos + 1, false);
        pos += 5;
        if (pos + len > body.length) {
            throw new WireError('truncated gRPC-web frame');
        }
        const payload = body.subarray(pos, pos + len);
        pos += len;
        if ((flag & FLAG_TRAILER) !== 0) {
            trailers = parseTrailers(textDecoder.decode(payload));
        } else {
            messages.push(payload.slice());
        }
    }
    return { messages, trailers };
}

/** Parse an HTTP/1.1-style trailer block (`key: value` lines) into a lowercased map. */
export function parseTrailers(block: string): Map<string, string> {
    const map = new Map<string, string>();
    for (const line of block.split(/\r?\n/)) {
        if (line.length === 0) {
            continue;
        }
        const idx = line.indexOf(':');
        if (idx === -1) {
            continue;
        }
        const key = line.slice(0, idx).trim().toLowerCase();
        const value = line.slice(idx + 1).trim();
        map.set(key, value);
    }
    return map;
}

/** Encode a UTF-8 string to bytes (request framing + test helper surface). */
export function utf8(value: string): Uint8Array {
    return textEncoder.encode(value);
}
