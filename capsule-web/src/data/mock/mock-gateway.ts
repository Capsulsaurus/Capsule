import type { Album, Asset } from '@/domain';
import type { CapsuleGateway } from '../gateway';

const randomInt = (min: number, max: number) =>
    Math.floor(Math.random() * (max - min + 1)) + min;
const randomId = () => Math.random().toString(36).substring(7);

const randomDate = (start: Date, end: Date) =>
    new Date(
        start.getTime() + Math.random() * (end.getTime() - start.getTime()),
    );

const CITIES = [
    'New York',
    'Tokyo',
    'London',
    'Paris',
    'Berlin',
    'San Francisco',
    'Sydney',
    undefined,
];

// Sample thumbhashes (base64).
const THUMBHASHES = [
    '1QcSHQRnh493V4dIh4eXh1h4kJY=', // Nature/Green
    'k0oGLQaSZ3l0hweJiIiHh1iAZ1Y=', // Warm/Red
    'ImYFHPZ3aHiHiHh4eIeXh4h4R4g=', // Sky/Blue
    'VFopSlCAhoh2iJh3eniHd3d2d2g=', // Gray/City
];

const generateAssets = (count: number): Asset[] =>
    Array.from({ length: count })
        .map(() => {
            const width = randomInt(400, 1600);
            const height = randomInt(400, 1600);
            return {
                id: randomId(),
                // Random placeholder imagery via picsum.
                url: `https://picsum.photos/seed/${randomId()}/${width}/${height}`,
                thumbnailUrl: `https://picsum.photos/seed/${randomId()}/400/${Math.floor(400 * (height / width))}`,
                date: randomDate(new Date(2023, 0, 1), new Date()),
                type: (Math.random() > 0.8
                    ? 'video'
                    : 'image') as Asset['type'],
                duration: Math.random() > 0.8 ? '0:15' : undefined,
                location: CITIES[randomInt(0, CITIES.length - 1)],
                width,
                height,
                thumbhash: THUMBHASHES[randomInt(0, THUMBHASHES.length - 1)],
            };
        })
        .sort((a, b) => b.date.getTime() - a.date.getTime());

const generateAlbums = (count: number): Album[] =>
    Array.from({ length: count }).map((_, i) => ({
        id: randomId(),
        title: `Album ${i + 1}`,
        coverUrl: `https://picsum.photos/seed/${randomId()}/300/300`,
        assetCount: randomInt(10, 500),
    }));

const assets = generateAssets(100);
const albums = generateAlbums(10);

/**
 * In-memory gateway backed by randomly generated sample data. Lets the read-only
 * UI run end-to-end before the real server/SDK adapter exists; selected via
 * `gateway` in ../index.ts.
 */
export const mockGateway: CapsuleGateway = {
    listAssets: async () => assets,
    listAlbums: async () => albums,
    getAlbum: async (id) => albums.find((album) => album.id === id) ?? null,
    getAlbumAssets: async (albumId) => {
        const album = albums.find((a) => a.id === albumId);
        return assets.slice(0, album?.assetCount ?? 0);
    },
};
