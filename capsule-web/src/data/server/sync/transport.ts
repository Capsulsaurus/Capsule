/**
 * The transport seam for the sync feed. `SyncStore` and `ServerGateway` depend only on
 * this interface, so the store logic is unit-tested against a mock transport with no live
 * server (S-D6 gate). The real implementation, `GrpcWebSyncTransport`, speaks gRPC-web
 * over `fetch` per the api-surfaces transport map.
 */

import {
    decodeSyncResponse,
    encodeSyncRequest,
    frameRequest,
    parseResponseFrames,
    type SyncRequest,
    type SyncResponse,
} from './wire';

/** A single `Sync` RPC. The cursor + page-size hint go in, one page comes back. */
export interface SyncTransport {
    sync(request: SyncRequest): Promise<SyncResponse>;
}

/**
 * A transport failure with the cross-transport `error.*` discriminator when the server
 * supplied one (`x-capsule-error-code`), plus the numeric gRPC status. A client switches
 * on `errorCode`, never on the transport status alone (api-surfaces Rejection Mapping).
 */
export class SyncTransportError extends Error {
    constructor(
        /** The numeric gRPC status code (0 = OK). */
        public readonly grpcStatus: number,
        /** The stable `error.*` code, when the server sent one. */
        public readonly errorCode: string | undefined,
        message: string,
    ) {
        super(message);
        this.name = 'SyncTransportError';
    }
}

/** Everything the gRPC-web transport needs, injected so it stays free of app globals. */
export interface GrpcWebTransportConfig {
    /**
     * Feed base URL up to (not including) the gRPC service path — the transport appends
     * `/capsule.sync.v1.SyncService/Sync`. In the deployed server this is
     * `${API_BASE}/v1/sync` (see `capsule-api::create_router`).
     */
    baseUrl: string;
    /** The `x-capsule-protocol` version the client speaks (`YYYY-MM-DD`). */
    protocol: string;
    /** Returns the current bearer access token, or `null` when unauthenticated. */
    accessToken: () => string | null;
    /** Injectable for tests; defaults to the global `fetch`. */
    fetchImpl?: typeof fetch;
}

const SERVICE_PATH = '/capsule.sync.v1.SyncService/Sync';
const CONTENT_TYPE = 'application/grpc-web+proto';
const MD_PROTOCOL = 'x-capsule-protocol';
const MD_ERROR_CODE = 'x-capsule-error-code';

/**
 * gRPC-web transport for the sync feed. One unary `Sync` call per page; the opaque cursor
 * carries resumption. The bearer token and `x-capsule-protocol` ride request metadata
 * exactly as the REST surfaces carry them (api-surfaces Negotiation Across Transports).
 */
export class GrpcWebSyncTransport implements SyncTransport {
    private readonly fetchImpl: typeof fetch;

    constructor(private readonly config: GrpcWebTransportConfig) {
        this.fetchImpl = config.fetchImpl ?? fetch;
    }

    async sync(request: SyncRequest): Promise<SyncResponse> {
        const body = frameRequest(encodeSyncRequest(request));
        const headers = new Headers({
            'content-type': CONTENT_TYPE,
            accept: CONTENT_TYPE,
            'x-grpc-web': '1',
            [MD_PROTOCOL]: this.config.protocol,
        });
        const token = this.config.accessToken();
        if (token) {
            headers.set('authorization', `Bearer ${token}`);
        }

        const res = await this.fetchImpl(
            `${this.config.baseUrl}${SERVICE_PATH}`,
            {
                method: 'POST',
                headers,
                body,
            },
        );

        // A gRPC-web status can arrive as HTTP headers (trailers-only) or in the trailer
        // frame within the body. Read the body first, then reconcile both sources.
        const raw = new Uint8Array(await res.arrayBuffer());
        const { messages, trailers } = parseResponseFrames(raw);

        const status = readStatus(res.headers, trailers);
        if (status.code !== 0) {
            throw new SyncTransportError(
                status.code,
                status.errorCode,
                status.message ?? `sync failed with gRPC status ${status.code}`,
            );
        }
        if (!res.ok) {
            throw new SyncTransportError(
                status.code,
                status.errorCode,
                `sync HTTP ${res.status}`,
            );
        }
        if (messages.length === 0) {
            throw new SyncTransportError(
                2,
                undefined,
                'sync response carried no message',
            );
        }
        return decodeSyncResponse(messages[0]);
    }
}

interface GrpcStatus {
    code: number;
    message?: string;
    errorCode?: string;
}

/** Resolve the gRPC status from response headers and/or the trailer block. */
function readStatus(
    headers: Headers,
    trailers: Map<string, string>,
): GrpcStatus {
    const rawCode =
        trailers.get('grpc-status') ?? headers.get('grpc-status') ?? undefined;
    const message =
        trailers.get('grpc-message') ??
        headers.get('grpc-message') ??
        undefined;
    const errorCode =
        trailers.get(MD_ERROR_CODE) ?? headers.get(MD_ERROR_CODE) ?? undefined;
    // Absent grpc-status on a 200 body-carrying response is treated as OK.
    const code = rawCode === undefined ? 0 : Number.parseInt(rawCode, 10);
    return {
        code: Number.isNaN(code) ? 2 : code,
        message: message ? decodeURIComponent(message) : undefined,
        errorCode,
    };
}
