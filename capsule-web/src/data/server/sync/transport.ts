/**
 * The transport seam for the sync feed. `SyncStore` and `ServerGateway` depend only on this
 * interface, so the store logic is unit-tested against a mock transport with no live server
 * (`S-D6` gate).
 *
 * The real implementation was `GrpcWebSyncTransport`, which framed a hand-rolled Protobuf
 * message into gRPC-web and reconciled a status that could arrive in headers *or* in a trailer
 * block. `S-C2` moved the feed onto REST and `S-C59` retired the gRPC surface entirely, so what
 * is left is [`RestSyncTransport`]: one `GET`, one JSON body, one status line.
 */

import {
    decodeSyncResponse,
    encodeSyncRequest,
    type SyncRequest,
    type SyncResponse,
} from './wire';

/** A single page fetch. The cursor + page-size hint go in, one page comes back. */
export interface SyncTransport {
    sync(request: SyncRequest): Promise<SyncResponse>;
}

/**
 * A transport failure with the cross-transport `error.*` discriminator when the server supplied
 * one, plus the HTTP status.
 *
 * A client switches on `errorCode`, never on the status alone (api-surfaces Rejection Mapping):
 * the server answers RFC 9457 problem documents whose `code` extension is the stable catalog
 * key, and that key is what a client can act on. The status says how, the code says what.
 */
export class SyncTransportError extends Error {
    constructor(
        /** The HTTP status the server answered with. */
        public readonly httpStatus: number,
        /** The stable `error.*` code, when the server sent one. */
        public readonly errorCode: string | undefined,
        message: string,
    ) {
        super(message);
        this.name = 'SyncTransportError';
    }
}

/** Everything the REST transport needs, injected so it stays free of app globals. */
export interface RestTransportConfig {
    /**
     * The API origin, up to but not including `/v1`. The transport appends `/v1/sync`.
     */
    baseUrl: string;
    /** The `x-capsule-protocol` version the client speaks (`YYYY-MM-DD`). */
    protocol: string;
    /** Returns the current bearer access token, or `null` when unauthenticated. */
    accessToken: () => string | null;
    /** Injectable for tests; defaults to the global `fetch`. */
    fetchImpl?: typeof fetch;
}

const FEED_PATH = '/v1/sync';
const MD_PROTOCOL = 'x-capsule-protocol';

/**
 * REST transport for the sync feed. One `GET` per page; the opaque cursor carries resumption.
 */
export class RestSyncTransport implements SyncTransport {
    private readonly fetchImpl: typeof fetch;

    constructor(private readonly config: RestTransportConfig) {
        this.fetchImpl = config.fetchImpl ?? fetch;
    }

    async sync(request: SyncRequest): Promise<SyncResponse> {
        const headers = new Headers({
            accept: 'application/json',
            [MD_PROTOCOL]: this.config.protocol,
        });
        const token = this.config.accessToken();
        if (token) {
            headers.set('authorization', `Bearer ${token}`);
        }

        const url = `${this.config.baseUrl}${FEED_PATH}${encodeSyncRequest(request)}`;
        const res = await this.fetchImpl(url, { method: 'GET', headers });

        if (!res.ok) {
            throw await problemError(res);
        }
        return decodeSyncResponse(await res.json());
    }
}

/**
 * Turn a non-2xx response into a [`SyncTransportError`], carrying the problem document's `code`
 * when there is one.
 *
 * Deliberately tolerant of a body that is not a problem document: a proxy, a load balancer or a
 * gateway timeout can answer on the server's behalf with HTML or with nothing at all, and a
 * transport that threw a `SyntaxError` there would report a JSON parse failure where the real
 * fault is "the server did not answer". The status is always available; the code is not.
 */
async function problemError(res: Response): Promise<SyncTransportError> {
    let code: string | undefined;
    let detail: string | undefined;
    try {
        const body = (await res.json()) as { code?: unknown; detail?: unknown };
        code = typeof body?.code === 'string' ? body.code : undefined;
        detail = typeof body?.detail === 'string' ? body.detail : undefined;
    } catch {
        // Not a problem document. The status still is one.
    }
    return new SyncTransportError(
        res.status,
        code,
        detail ?? `sync failed with HTTP ${res.status}`,
    );
}
