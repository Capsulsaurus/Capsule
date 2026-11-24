import { describe, expect, it } from 'vitest';
import { AUDIT_STATUSES } from './page-audit.mjs';
import {
    badgeSidebar,
    routeKey,
    withLocaleFallbacks,
} from './sidebar-status.mjs';
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

const UNAUDITED = { text: 'Unaudited', variant: 'caution' };

describe('titleBadge / navBadge', () => {
    it('titles show every computed status', () => {
        expect(titleBadge('unaudited')).toEqual(UNAUDITED);
        expect(titleBadge('audited')).toEqual({
            text: 'Audited',
            variant: 'success',
        });
        expect(titleBadge('outdated')).toEqual({
            text: 'Changed since audit',
            variant: 'caution',
        });
    });

    it('covers every status in AUDIT_STATUSES', () => {
        for (const status of AUDIT_STATUSES) {
            expect(titleBadge(status)).toBeDefined();
        }
    });

    it('navigation hides audited and shows the rest', () => {
        expect(navBadge('audited')).toBeUndefined();
        expect(navBadge('unaudited')).toEqual(UNAUDITED);
        expect(navBadge('outdated')).toEqual({
            text: 'Changed',
            variant: 'caution',
        });
    });

    it('unknown or missing statuses show nothing', () => {
        for (const s of [
            undefined,
            '',
            'toString',
            'review',
            'draft',
            'stable',
        ]) {
            expect(titleBadge(s)).toBeUndefined();
            expect(navBadge(s)).toBeUndefined();
        }
    });

    it('returns a fresh object each call', () => {
        const a = titleBadge('unaudited');
        a.text = 'mutated';
        expect(titleBadge('unaudited')).toEqual(UNAUDITED);
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
        ['', 'unaudited'],
        ['design', 'audited'],
        ['design/clients', 'unaudited'],
        ['guides/self-hosting', 'unaudited'],
    ]);

    it('badges unaudited links and leaves audited links bare', () => {
        const [audited, unaudited] = badgeSidebar(
            [link('/design/'), link('/design/clients/')],
            statuses,
        );
        expect(audited.badge).toBeUndefined();
        expect(unaudited.badge).toEqual(UNAUDITED);
    });

    it('recurses into nested groups without badging the groups', () => {
        const [outer] = badgeSidebar(
            [group([group([link('/guides/self-hosting/')])])],
            statuses,
        );
        expect(outer.badge).toBeUndefined();
        expect(outer.entries[0].badge).toBeUndefined();
        expect(outer.entries[0].entries[0].badge).toEqual(UNAUDITED);
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
        const byId = new Map([[routeKey('index'), 'unaudited']]);
        expect(badgeSidebar([link('/')], byId)[0].badge).toEqual(UNAUDITED);
    });

    it('does not mutate its input', () => {
        const input = [group([link('/design/clients/')])];
        const snapshot = structuredClone(input);
        badgeSidebar(input, statuses);
        expect(input).toEqual(snapshot);
    });
});

describe('withLocaleFallbacks', () => {
    const english = new Map([
        ['', 'unaudited'],
        ['design/clients', 'audited'],
        ['guides/start', 'unaudited'],
        // A translation's own status.
        ['ja/guides/start', 'audited'],
        ['ja', 'audited'],
    ]);
    const statuses = withLocaleFallbacks(english, ['ja', 'ko']);

    it("gives a locale's untranslated routes the English page's status", () => {
        expect(statuses.get('ja/design/clients')).toBe('audited');
        expect(statuses.get('ko/guides/start')).toBe('unaudited');
        expect(statuses.get('ko')).toBe('unaudited');
    });

    it("keeps a translation's own status", () => {
        expect(statuses.get('ja/guides/start')).toBe('audited');
        expect(statuses.get('ja')).toBe('audited');
    });

    it('never treats a translation as English to copy', () => {
        expect(statuses.has('ko/ja/guides/start')).toBe(false);
        expect(statuses.has('ko/ja')).toBe(false);
    });

    it('badges a fallback route in the sidebar', () => {
        expect(
            badgeSidebar([link('/ko/guides/start/')], statuses)[0].badge,
        ).toEqual(UNAUDITED);
    });
});
