import type { CapsuleGateway } from '../gateway';

/**
 * Placeholder for the real data source.
 *
 * The capsule-api server is mid-rewrite to the end-to-end-encrypted, key-free
 * model described in the design docs, so there is no stable schema to implement
 * against yet. When there is, this adapter implements `CapsuleGateway` over the
 * high-level capsule-sdk / server APIs (likely GraphQL for library queries plus
 * REST for content-addressed blobs), and ../index.ts selects it (e.g. behind a
 * PUBLIC_DATA_SOURCE env flag).
 */
const notImplemented = (): never => {
    throw new Error(
        'ServerGateway is not implemented yet: pending the capsule-api E2E rework.',
    );
};

export const serverGateway: CapsuleGateway = {
    listAssets: notImplemented,
    listAlbums: notImplemented,
    getAlbum: notImplemented,
    getAlbumAssets: notImplemented,
};
