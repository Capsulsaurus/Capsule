import type { CapsuleGateway } from '../gateway';

/**
 * Placeholder for the real data source.
 *
 * The capsule-api server is mid-rewrite to the end-to-end-encrypted, key-free
 * model described in the design docs, so there is no stable schema to implement
 * against yet. When there is, this adapter implements `CapsuleGateway` as the
 * browser's client-side read path (slice S-D6 in the repo-root SLICES.md): a
 * sync-fed local store queried in the client — the web analogue of
 * library.sqlite — plus ranged REST fetches for content-addressed blobs. There
 * is no GraphQL: rich queries have no server surface at all (api-surfaces
 * design doc). ../index.ts selects it (e.g. behind a PUBLIC_DATA_SOURCE flag).
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
