import { describe, expect, it, vi } from 'vitest';
import {
    currentLabel,
    expectsMenu,
    menuEntries,
    moveFocus,
    placement,
    readCachedVersions,
    reserveScript,
    VERSIONS_CACHE_KEY,
    versionsForMenu,
    writeCachedVersions,
} from './version-menu.mjs';

describe('currentLabel', () => {
    it('names the latest build and a release', () => {
        expect(currentLabel('')).toBe('Latest');
        expect(currentLabel('0.3')).toBe('v0.3');
    });
});

describe('menuEntries', () => {
    it('offers nothing while no version is released', () => {
        expect(menuEntries({ versions: [] }, '', '/design/', '/')).toEqual([]);
        expect(menuEntries(null, '', '/design/', '/')).toEqual([]);
    });

    it('lists Latest, then each release newest first, the same page in each', () => {
        expect(
            menuEntries({ versions: ['0.1', '0.2'] }, '', '/design/sync/', '/'),
        ).toEqual([
            {
                label: 'Latest',
                base: '/',
                href: '/design/sync/',
                current: true,
            },
            {
                label: 'v0.2',
                base: '/v/0.2/',
                href: '/v/0.2/design/sync/',
                current: false,
            },
            {
                label: 'v0.1',
                base: '/v/0.1/',
                href: '/v/0.1/design/sync/',
                current: false,
            },
        ]);
    });

    it('moves a release page out of its own base', () => {
        const [latest, release] = menuEntries(
            { versions: ['0.1'] },
            '0.1',
            '/v/0.1/design/sync/',
            '/v/0.1/',
        );
        expect(latest).toMatchObject({ href: '/design/sync/', current: false });
        expect(release).toMatchObject({
            href: '/v/0.1/design/sync/',
            current: true,
        });
    });

    it('names a release build itself before its release is listed', () => {
        expect(
            menuEntries({ versions: ['0.1'] }, '0.2', '/v/0.2/', '/v/0.2/').map(
                (entry) => [entry.label, entry.current],
            ),
        ).toEqual([
            ['Latest', false],
            ['v0.2', true],
            ['v0.1', false],
        ]);
    });
});

describe('moveFocus', () => {
    it('steps through the list and wraps at either end', () => {
        expect(moveFocus('ArrowDown', 0, 3)).toBe(1);
        expect(moveFocus('ArrowDown', 2, 3)).toBe(0);
        expect(moveFocus('ArrowUp', 0, 3)).toBe(2);
        expect(moveFocus('ArrowUp', 1, 3)).toBe(0);
    });

    it('jumps to either end', () => {
        expect(moveFocus('Home', 2, 3)).toBe(0);
        expect(moveFocus('End', 0, 3)).toBe(2);
    });

    it('enters the list from outside it', () => {
        expect(moveFocus('ArrowDown', -1, 3)).toBe(0);
        expect(moveFocus('ArrowUp', -1, 3)).toBe(2);
    });

    it('ignores every other key and an empty list', () => {
        expect(moveFocus('Enter', 0, 3)).toBeNull();
        expect(moveFocus('ArrowDown', 0, 0)).toBeNull();
    });
});

describe('placement', () => {
    it('opens below when the menu fits there', () => {
        expect(placement(200, 0, 120)).toBe('below');
    });

    it('opens above when only that side fits', () => {
        expect(placement(80, 400, 120)).toBe('above');
    });

    it('takes the roomier side when neither fits', () => {
        expect(placement(80, 100, 120)).toBe('above');
        expect(placement(100, 80, 120)).toBe('below');
    });
});

/** A Storage stand-in over a Map. */
function memoryStorage() {
    const map = new Map();
    return {
        getItem: (key) => (map.has(key) ? map.get(key) : null),
        setItem: (key, value) => map.set(key, String(value)),
    };
}

describe('version cache', () => {
    it('round-trips the list through storage', () => {
        const storage = memoryStorage();
        expect(readCachedVersions(storage)).toBeUndefined();
        writeCachedVersions(storage, { versions: ['0.1'] });
        expect(readCachedVersions(storage)).toEqual({ versions: ['0.1'] });
    });

    it('treats missing, unparseable or refused storage as no cache', () => {
        expect(readCachedVersions(null)).toBeUndefined();
        expect(
            readCachedVersions({ getItem: () => '{not json' }),
        ).toBeUndefined();
        expect(
            readCachedVersions({
                getItem: () => {
                    throw new Error('SecurityError');
                },
            }),
        ).toBeUndefined();
    });

    it('drops a write that storage refuses', () => {
        expect(() =>
            writeCachedVersions(
                {
                    setItem: () => {
                        throw new Error('QuotaExceededError');
                    },
                },
                { versions: [] },
            ),
        ).not.toThrow();
    });
});

describe('versionsForMenu', () => {
    it('serves the cached list at once and refreshes the cache behind it', async () => {
        const storage = memoryStorage();
        writeCachedVersions(storage, { versions: ['0.1'] });
        let release;
        const fetched = new Promise((resolve) => {
            release = resolve;
        });
        const fetchList = vi.fn(() => fetched);
        await expect(versionsForMenu(storage, fetchList)).resolves.toEqual({
            versions: ['0.1'],
        });
        expect(fetchList).toHaveBeenCalledTimes(1);
        // A release published mid-session: the next page sees it.
        release({ versions: ['0.2', '0.1'] });
        await fetched;
        await Promise.resolve();
        expect(readCachedVersions(storage)).toEqual({
            versions: ['0.2', '0.1'],
        });
    });

    it('keeps the stale copy when the refresh fails', async () => {
        const storage = memoryStorage();
        writeCachedVersions(storage, { versions: ['0.1'] });
        const failed = Promise.reject(new Error('offline'));
        await expect(versionsForMenu(storage, () => failed)).resolves.toEqual({
            versions: ['0.1'],
        });
        await failed.catch(() => {});
        expect(readCachedVersions(storage)).toEqual({ versions: ['0.1'] });
    });

    it('waits for the fetch and caches it when nothing is cached', async () => {
        const storage = memoryStorage();
        await expect(
            versionsForMenu(storage, async () => ({ versions: ['0.3'] })),
        ).resolves.toEqual({ versions: ['0.3'] });
        expect(readCachedVersions(storage)).toEqual({ versions: ['0.3'] });
    });

    it('resolves to undefined when nothing is cached and the fetch fails', async () => {
        const storage = memoryStorage();
        await expect(
            versionsForMenu(storage, async () => {
                throw new Error('offline');
            }),
        ).resolves.toBeUndefined();
        expect(readCachedVersions(storage)).toBeUndefined();
    });
});

describe('expectsMenu', () => {
    it('always expects one on a release build', () => {
        expect(expectsMenu('0.3', null)).toBe(true);
    });

    it('expects one on the latest build once the cache names a release', () => {
        expect(expectsMenu('', '{"versions":["0.1"]}')).toBe(true);
        expect(expectsMenu('', '{"versions":[]}')).toBe(false);
        expect(expectsMenu('', '{"versions":["latest"]}')).toBe(false);
    });

    it('guesses none with no cache or a bad one', () => {
        expect(expectsMenu('', null)).toBe(false);
        expect(expectsMenu('', '{not json')).toBe(false);
        expect(expectsMenu('', 'null')).toBe(false);
    });

    it('agrees with menuEntries on whether a menu exists', () => {
        for (const versions of [[], ['0.1'], ['x'], ['0.1', 'x']]) {
            const json = { versions };
            expect(expectsMenu('', JSON.stringify(json))).toBe(
                menuEntries(json, '', '/', '/').length > 0,
            );
        }
    });
});

describe('reserveScript', () => {
    /** Runs the inline script against a fake element and storage. */
    function run(current, cached) {
        const el = { dataset: { current } };
        const fn = new Function('document', 'sessionStorage', reserveScript());
        fn(
            { currentScript: { previousElementSibling: el } },
            { getItem: (key) => (key === VERSIONS_CACHE_KEY ? cached : null) },
        );
        return 'expectMenu' in el.dataset;
    }

    it('marks the switcher before its module runs', () => {
        expect(run('', '{"versions":["0.1"]}')).toBe(true);
        expect(run('0.2', null)).toBe(true);
        expect(run('', null)).toBe(false);
    });
});
