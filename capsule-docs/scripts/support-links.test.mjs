// The support page (src/content/docs/support.mdx) and the issue template
// chooser (.github/ISSUE_TEMPLATE/) route every kind of request to a
// destination. The link validator checks only the site's own pages, so this
// test keeps the rest from rotting: every issue form the page names exists,
// every discussion category it names is in the mirrored list below, each kind
// of request has its one front door, every docs-site URL the templates name
// resolves to a page, and no route leads a vulnerability into a public issue.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { REPO } from '../src/lib/site-nav.mjs';
import { linkTargets } from './lib/markdown.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const DOCS = join(ROOT, 'capsule-docs/src/content/docs');
const PAGE = join(DOCS, 'support.mdx');
const TEMPLATES = join(ROOT, '.github/ISSUE_TEMPLATE');
const SITE = 'https://capsule.justinchung.net';

// The repository's discussion categories, by slug. GitHub has no file that
// declares them, so this list mirrors the repository settings by hand: a
// category renamed on GitHub is not caught here. Linking a new category is a
// deliberate change to this list.
const DISCUSSION_CATEGORIES = new Set([
    'announcements',
    'general',
    'ideas',
    'polls',
    'q-a',
]);

const links = linkTargets(readFileSync(PAGE, 'utf8')).map(
    (link) => link.target,
);
const github = links.filter((href) => href.startsWith('https://github.com/'));
const templateFiles = readdirSync(TEMPLATES);
const templateSources = templateFiles.map((name) => [
    name,
    readFileSync(join(TEMPLATES, name), 'utf8'),
]);

/** True when a site-relative path is served by an authored docs page. */
function hasPage(path) {
    const slug = path.replace(/[#?].*$/, '').replace(/^\/|\/$/g, '');
    return ['.md', '.mdx', '/index.md', '/index.mdx'].some((ext) =>
        existsSync(join(DOCS, `${slug}${ext}`)),
    );
}

describe('support page links', () => {
    it('links into the repository, or to the maintainer sponsor page', () => {
        for (const href of github) {
            expect(
                href.startsWith(`${REPO}/`) ||
                    href.startsWith('https://github.com/sponsors/'),
                href,
            ).toBe(true);
        }
    });

    it.each(
        github
            .map((href) => new URL(href).searchParams.get('template'))
            .filter(Boolean),
    )('issue form %s exists', (template) => {
        expect(templateFiles).toContain(template);
    });

    it.each(
        github
            .map((href) => href.match(/\/discussions\/categories\/([^/?#]+)/))
            .filter(Boolean)
            .map((match) => match[1]),
    )('discussion category %s is a known one', (slug) => {
        expect(DISCUSSION_CATEGORIES.has(slug)).toBe(true);
    });

    // Each kind of request has one front door. Q&A is also suggested from
    // the topic cards, so it is checked for presence, not uniqueness.
    it.each([
        ['bug report', `${REPO}/issues/new?template=bug_report.yml`],
        ['idea', `${REPO}/discussions/categories/ideas`],
        ['vulnerability', `${REPO}/security/advisories/new`],
        [
            'translation error',
            `${REPO}/issues/new?template=translation-error.md`,
        ],
        ['language request', `${REPO}/issues/new?template=language-request.md`],
    ])('routes a %s to one destination', (_, href) => {
        expect(links.filter((link) => link === href)).toHaveLength(1);
    });

    it('routes questions to Q&A', () => {
        expect(links).toContain(`${REPO}/discussions/categories/q-a`);
    });
});

describe('issue template chooser', () => {
    const siteUrls = templateSources.flatMap(([name, source]) =>
        [
            ...source.matchAll(
                /https:\/\/capsule\.justinchung\.net(\/[^\s)"']*)/g,
            ),
        ].map((match) => [name, match[1]]),
    );

    it('links to the docs site', () => {
        expect(siteUrls.length).toBeGreaterThan(0);
    });

    it.each(siteUrls)('%s links %s, an authored docs page', (_, path) => {
        expect(hasPage(path)).toBe(true);
    });

    it('names the domain the docs site is built for', () => {
        const config = readFileSync(
            join(ROOT, 'capsule-docs/astro.config.mjs'),
            'utf8',
        );
        expect(config).toContain(`site: '${SITE}'`);
    });

    it('routes a vulnerability to a private advisory', () => {
        const config = readFileSync(join(TEMPLATES, 'config.yml'), 'utf8');
        expect(config).toContain(`${REPO}/security/advisories/new`);
    });

    it('offers no public issue template for a vulnerability', () => {
        expect(templateFiles.filter((name) => /security/i.test(name))).toEqual(
            [],
        );
        expect(
            github.some((href) => /template=[^&]*security/i.test(href)),
        ).toBe(false);
    });
});
