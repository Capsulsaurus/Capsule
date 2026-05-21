export type Asset = {
    id: string;
    url: string;
    thumbnailUrl: string;
    date: Date;
    type: 'image' | 'video';
    duration?: string; // For videos
    location?: string;
    width: number;
    height: number;
    thumbhash: string;
};

export type Album = {
    id: string;
    title: string;
    coverUrl: string;
    assetCount: number;
};

const randomInt = (min: number, max: number) =>
    Math.floor(Math.random() * (max - min + 1)) + min;
const randomId = () => Math.random().toString(36).substring(7);

const randomDate = (start: Date, end: Date) => {
    return new Date(
        start.getTime() + Math.random() * (end.getTime() - start.getTime()),
    );
};

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

// Sample thumbhashes (base64)
const THUMBHASHES = [
    '1QcSHQRnh493V4dIh4eXh1h4kJY=', // Nature/Green
    'k0oGLQaSZ3l0hweJiIiHh1iAZ1Y=', // Warm/Red
    'ImYFHPZ3aHiHiHh4eIeXh4h4R4g=', // Sky/Blue
    'VFopSlCAhoh2iJh3eniHd3d2d2g=', // Gray/City
];

export const generateAssets = (count: number): Asset[] => {
    return Array.from({ length: count })
        .map(() => {
            const width = randomInt(400, 1600);
            const height = randomInt(400, 1600);
            return {
                id: randomId(),
                // using picsum for images
                url: `https://picsum.photos/seed/${randomId()}/${width}/${height}`,
                thumbnailUrl: `https://picsum.photos/seed/${randomId()}/400/${Math.floor(400 * (height / width))}`,
                date: randomDate(new Date(2023, 0, 1), new Date()),
                type: (Math.random() > 0.8 ? 'video' : 'image') as
                    | 'video'
                    | 'image',
                duration: Math.random() > 0.8 ? '0:15' : undefined,
                location: CITIES[randomInt(0, CITIES.length - 1)],
                width,
                height,
                thumbhash: THUMBHASHES[randomInt(0, THUMBHASHES.length - 1)],
            };
        })
        .sort((a, b) => b.date.getTime() - a.date.getTime());
};

export const generateAlbums = (count: number): Album[] => {
    return Array.from({ length: count }).map((_, i) => ({
        id: randomId(),
        title: `Album ${i + 1}`,
        coverUrl: `https://picsum.photos/seed/${randomId()}/300/300`,
        assetCount: randomInt(10, 500),
    }));
};

export const mockAssets = generateAssets(100);
export const mockAlbums = generateAlbums(10);
