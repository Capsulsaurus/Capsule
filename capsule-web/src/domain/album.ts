/**
 * An album as the web client renders it in lists and headers.
 *
 * Thin display shape; the real adapter maps capsule-core's album model
 * (membership, epoch/MLS state, authorization) down to this. See the `Asset`
 * type for the rationale.
 */
export interface Album {
    /** Stable album id (capsule-core: `Album.id`). */
    id: string;
    /** Display title (capsule-core: album metadata). */
    title: string;
    /** Cover image URL (server: cover asset thumbnail). */
    coverUrl: string;
    /** Number of assets in the album. */
    assetCount: number;
}
