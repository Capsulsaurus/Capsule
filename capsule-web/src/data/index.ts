import type { CapsuleGateway } from './gateway';
import { createBrowserServerGateway } from './server/server-gateway';

/**
 * The active data source the UI reads through: the real key-free server gateway (slice
 * S-D6). It consumes the `capsule.sync.v1` feed into a client-side store and answers reads
 * from it — the browser's `library.sqlite` analogue. The mock gateway is retired.
 *
 * Reads return key-free shells: ids, album membership/counts and awaiting-original state are
 * real; titles, cover art, capture dates, dimensions, LQIP and locations live in encrypted
 * metadata and are absent until a wasm decode/verify boundary lands beneath this gateway
 * (gateway.ts). With no reachable server the store stays empty and the app renders empty
 * states rather than failing.
 */
export const gateway: CapsuleGateway = createBrowserServerGateway();
