import { useQuery } from '@tanstack/react-query';
import { gateway } from './index';

/** Stable React Query keys for the gateway reads. */
export const queryKeys = {
    assets: ['assets'] as const,
    albums: ['albums'] as const,
    album: (id: string) => ['albums', id] as const,
    albumAssets: (id: string) => ['albums', id, 'assets'] as const,
};

/** All assets, newest first. */
export function useAssets() {
    return useQuery({
        queryKey: queryKeys.assets,
        queryFn: () => gateway.listAssets(),
    });
}

/** All albums. */
export function useAlbums() {
    return useQuery({
        queryKey: queryKeys.albums,
        queryFn: () => gateway.listAlbums(),
    });
}

/** A single album by id (data is null when not found). */
export function useAlbum(id: string) {
    return useQuery({
        queryKey: queryKeys.album(id),
        queryFn: () => gateway.getAlbum(id),
    });
}

/** The assets in an album. */
export function useAlbumAssets(id: string) {
    return useQuery({
        queryKey: queryKeys.albumAssets(id),
        queryFn: () => gateway.getAlbumAssets(id),
    });
}
