import type { Album, Asset } from '@/domain';

/**
 * The read-only boundary between the web UI and a Capsule data source.
 *
 * The web client holds NO business logic: validation, decryption, sync and the
 * `verify_asset` chokepoint all live in capsule-core and are surfaced to clients
 * through high-level APIs (capsule-sdk / the capsule-api server). This interface
 * is where those APIs plug in. Today it is backed by the mock gateway; when the
 * server schema is live a ServerGateway implements the same contract. If
 * capsule-core later ships a wasm build, a decode/verify boundary slots in
 * *below* this one (assets would arrive as ciphertext refs plus a `decode()`
 * call) without changing this interface.
 *
 * It is intentionally read-only: per the design docs the web client cannot
 * author assets, edit metadata, or run lifecycle transitions — those require the
 * hardware-bound and write-tier keys a browser does not have. New read methods
 * (search, lifecycle filters for trash/archive/favorites) are added here as the
 * UI needs them.
 */
export interface CapsuleGateway {
    /** All assets visible to the session, newest first. */
    listAssets(): Promise<Asset[]>;
    /** All albums visible to the session. */
    listAlbums(): Promise<Album[]>;
    /** A single album by id, or null if it does not exist / is not visible. */
    getAlbum(id: string): Promise<Album | null>;
    /** The assets belonging to an album. */
    getAlbumAssets(albumId: string): Promise<Asset[]>;
}
