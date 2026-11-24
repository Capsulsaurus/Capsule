import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
    ariaCurrent,
    currentLink,
    footerLinks,
    headerLinks,
    isActive,
    localizeLink,
    REPO,
} from './site-nav.mjs';

const DOCS = fileURLToPath(new URL('../content/docs/', import.meta.url));

/** True when a site-relative href is served by an authored docs page. */
function hasPage(href) {
    const slug = href.replace(/^\/|\/$/g, '');
    return ['.md', '.mdx', '/index.md', '/index.mdx'].some((ext) =>
        existsSync(`${DOCS}${slug}${ext}`),
    );
}

describe('headerLinks', () => {
    it('lists Guides, Design, Reference and Blog, in that order', () => {
        expect(
            headerLinks({ blogIsLive: true }).map((link) => link.label),
        ).toEqual(['Guides', 'Design', 'Reference', 'Blog']);
    });

    it('omits Blog until the blog is live', () => {
        expect(
            headerLinks({ blogIsLive: false }).map((link) => link.label),
        ).toEqual(['Guides', 'Design', 'Reference']);
    });

    it('gives every link a section', () => {
        for (const link of headerLinks({ blogIsLive: true })) {
            expect(link.section).toMatch(/^\/[a-z-]+\/$/);
            expect(link.href.startsWith(link.section)).toBe(true);
        }
    });
});

describe('footerLinks', () => {
    it('lists the jump links in order, with Blog only when live', () => {
        expect(
            footerLinks({ blogIsLive: true }).map((link) => link.label),
        ).toEqual([
            'Documentation',
            'Blog',
            'Roadmap',
            'Discussions',
            'Downloads',
            'Support',
            'GitHub',
        ]);
        expect(
            footerLinks({ blogIsLive: false }).map((link) => link.label),
        ).toEqual([
            'Documentation',
            'Roadmap',
            'Discussions',
            'Downloads',
            'Support',
            'GitHub',
        ]);
    });

    it('marks exactly the off-site links external', () => {
        for (const link of footerLinks({ blogIsLive: true })) {
            expect(Boolean(link.external)).toBe(link.href.startsWith(REPO));
        }
    });
});

describe('internal links', () => {
    const internal = [
        ...headerLinks({ blogIsLive: true }),
        ...footerLinks({ blogIsLive: true }),
    ].filter((link) => !link.external);

    it.each(
        internal.map((link) => [link.label, link.href]),
    )('%s (%s) is a trailing-slash route', (_, href) => {
        expect(href).toMatch(/^\/([a-z-]+\/)+$/);
    });

    // The blog index is a plugin route with no authored page; every other
    // internal link must resolve to one, since the link validator does not
    // see these components.
    it.each(
        internal
            .filter((link) => link.href !== '/blog/')
            .map((link) => [link.label, link.href]),
    )('%s (%s) resolves to an authored page', (_, href) => {
        expect(hasPage(href)).toBe(true);
    });
});

describe('isActive', () => {
    const [guides, design, reference] = headerLinks({ blogIsLive: false });

    it.each([
        [design, '/design/', true],
        [design, '/design/cryptography/keys/', true],
        [design, '/design', true],
        [design, '/designer/', false],
        [design, '/', false],
        [guides, '/guides/self-hosting/', true],
        [guides, '/design/', false],
        [reference, '/reference/api/upload/', true],
    ])('%o on %s -> %s', (link, pathname, expected) => {
        expect(isActive(link, pathname)).toBe(expected);
    });

    it('never marks a link without a section', () => {
        const [documentation] = footerLinks({ blogIsLive: false });
        expect(isActive(documentation, '/guides/getting-started/')).toBe(false);
    });
});

describe('ariaCurrent', () => {
    const [guides, design] = headerLinks({ blogIsLive: false });

    it.each([
        [design, '/design/', 'page'],
        [design, '/design', 'page'],
        [design, '/design/cryptography/keys/', 'true'],
        [guides, '/guides/getting-started/', 'page'],
        [guides, '/guides/self-hosting/', 'true'],
        [design, '/guides/', undefined],
    ])('%o on %s -> %s', (link, pathname, expected) => {
        expect(ariaCurrent(link, pathname)).toBe(expected);
    });
});

describe('currentLink', () => {
    const links = headerLinks({ blogIsLive: true });

    it.each([
        ['/design/cryptography/keys/', 'Design'],
        ['/guides/getting-started/', 'Guides'],
        ['/reference/api/upload/', 'Reference'],
        ['/blog/first-post/', 'Blog'],
        ['/', undefined],
        ['/support/', undefined],
        ['/designer/', undefined],
    ])('%s -> %s', (pathname, label) => {
        expect(currentLink(links, pathname)?.label).toBe(label);
    });
});

describe("on a locale's routes", () => {
    const links = headerLinks({ blogIsLive: true, locale: 'ja' });

    it('keeps every section link and its section in the locale, labelled in its language', () => {
        expect(links).toEqual([
            {
                label: 'ガイド',
                href: '/ja/guides/getting-started/',
                section: '/ja/guides/',
            },
            { label: '設計', href: '/ja/design/', section: '/ja/design/' },
            {
                label: 'リファレンス',
                href: '/ja/reference/',
                section: '/ja/reference/',
            },
            { label: 'ブログ', href: '/ja/blog/', section: '/ja/blog/' },
        ]);
    });

    it('marks the current section from the localized path', () => {
        expect(currentLink(links, '/ja/design/cryptography/')?.label).toBe(
            '設計',
        );
        expect(ariaCurrent(links[0], '/ja/guides/getting-started/')).toBe(
            'page',
        );
        // An English page is not in the locale's sections.
        expect(currentLink(links, '/design/')).toBeUndefined();
        expect(currentLink(links, '/ja/')).toBeUndefined();
    });

    it("keeps the footer's site links in the locale and its external links as they are", () => {
        expect(
            footerLinks({ blogIsLive: false, locale: 'ar' }).map(
                ({ label, href }) => [label, href],
            ),
        ).toEqual([
            ['التوثيق', '/ar/guides/getting-started/'],
            ['خارطة الطريق', `${REPO}/blob/master/ROADMAP.md`],
            ['النقاشات', `${REPO}/discussions`],
            ['التنزيلات', '/ar/downloads/'],
            ['الدعم', '/ar/support/'],
            ['GitHub', REPO],
        ]);
    });

    it('leaves the English links unchanged', () => {
        const link = { label: 'Design', href: '/design/', section: '/design/' };
        expect(localizeLink(link, undefined)).toBe(link);
    });
});
