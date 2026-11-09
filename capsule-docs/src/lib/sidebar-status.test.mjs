import { describe, expect, it } from 'vitest';
import { badgeSidebar, routeKey } from './sidebar-status.mjs';
import { navBadge, titleBadge } from './status-badge.mjs';

const link = (href, extra = {}) => ({
    type: 'link',
    label: href,
    href,
    isCurrent: false,
    badge: undefined,
    attrs: {},
    ...extra,
});
const group = (entries) => ({
    type: 'group',
    label: 'G',
    entries,
    collapsed: false,
    badge: undefined,
});

const DRAFT = { text: 'Draft', variant: 'caution' };

describe('titleBadge / navBadge', () => {
    it('titles show every known status', () => {
        expect(titleBadge('draft')).toEqual(DRAFT);
        expect(titleBadge('stable')).toEqual({
            text: 'Stable',
            variant: 'success',
        });
    });

    it('navigation hides stable and shows draft', () => {
        expect(navBadge('stable')).toBeUndefined();
        expect(navBadge('draft')).toEqual(DRAFT);
    });

    it('unknown or missing statuses show nothing', () => {
        for (const s of [undefined, '', 'toString', 'review']) {
            expect(titleBadge(s)).toBeUndefined();
            expect(navBadge(s)).toBeUndefined();
        }
    });

    it('returns a fresh object each call', () => {
        const a = titleBadge('draft');
        a.text = 'mutated';
        expect(titleBadge('draft')).toEqual(DRAFT);
    });
});

describe('routeKey', () => {
    it.each([
        ['/design/clients/', 'design/clients'],
        ['design/clients', 'design/clients'],
        ['/design/clients/#anchor', 'design/clients'],
        ['/design/?q=1', 'design'],
        ['/', ''],
        ['index', ''],
        ['design/index', 'design'],
        ['/reindex/', 'reindex'],
    ])('%s -> %s', (input, expected) => {
        expect(routeKey(input)).toBe(expected);
    });
});

describe('badgeSidebar', () => {
    const statuses = new Map([
        ['', 'draft'],
        ['design', 'stable'],
        ['design/clients', 'draft'],
        ['guides/self-hosting', 'draft'],
    ]);

    it('badges draft links and leaves stable links bare', () => {
        const [stable, draft] = badgeSidebar(
            [link('/design/'), link('/design/clients/')],
            statuses,
        );
        expect(stable.badge).toBeUndefined();
        expect(draft.badge).toEqual(DRAFT);
    });

    it('recurses into nested groups without badging the groups', () => {
        const [outer] = badgeSidebar(
            [group([group([link('/guides/self-hosting/')])])],
            statuses,
        );
        expect(outer.badge).toBeUndefined();
        expect(outer.entries[0].badge).toBeUndefined();
        expect(outer.entries[0].entries[0].badge).toEqual(DRAFT);
    });

    it('keeps an existing badge', () => {
        const own = { text: 'New', variant: 'tip' };
        const [entry] = badgeSidebar(
            [link('/design/clients/', { badge: own })],
            statuses,
        );
        expect(entry.badge).toBe(own);
    });

    it('ignores external links and routes with no known status', () => {
        const entries = [
            link('https://github.com/Capsulsaurus/Capsule'),
            link('//example.com/design/clients/'),
            link('mailto:someone@example.com'),
            link('/reference/unknown/'),
        ];
        for (const entry of badgeSidebar(entries, statuses)) {
            expect(entry.badge).toBeUndefined();
        }
    });

    it('matches the home page by its content id', () => {
        const byId = new Map([[routeKey('index'), 'draft']]);
        expect(badgeSidebar([link('/')], byId)[0].badge).toEqual(DRAFT);
    });

    it('does not mutate its input', () => {
        const input = [group([link('/design/clients/')])];
        const snapshot = structuredClone(input);
        badgeSidebar(input, statuses);
        expect(input).toEqual(snapshot);
    });
});
