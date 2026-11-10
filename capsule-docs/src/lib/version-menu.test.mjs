import { describe, expect, it } from 'vitest';
import {
    currentLabel,
    menuEntries,
    moveFocus,
    placement,
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
