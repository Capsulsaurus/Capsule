import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
// The matcher `starlight-llms-txt` filters with, so a glob means here what it means there.
import micromatch from 'micromatch';
import { describe, expect, it } from 'vitest';
import { walkFiles } from '../../scripts/lib/walk.mjs';
import { referenceSidebar } from '../../scripts/reference-groups.mjs';
import {
    LLMS_DETAILS,
    LLMS_PROMOTE,
    LLMS_SETS,
    LLMS_SMALL_EXCLUDE,
} from './llms-sets.mjs';

const REPO_ROOT = fileURLToPath(new URL('../../../', import.meta.url));
const CONTENT_PREFIX = 'capsule-docs/src/content/docs/';

/** `design/cryptography/index.md` -> `design/cryptography`, as Starlight ids it. */
function pageId(relPath) {
    const id = relPath
        .slice(CONTENT_PREFIX.length)
        .replace(/\.mdx?$/, '')
        .replace(/(^|\/)index$/, '');
    return id === '' ? 'index' : id;
}

/** Sidebar slugs, recursively: the generated reference pages exist on no CI runner. */
function sidebarSlugs(items) {
    return items.flatMap((item) =>
        'items' in item ? sidebarSlugs(item.items) : [item.slug],
    );
}

/** A page's frontmatter block, or '' when it has none. */
function frontmatter(source) {
    return /^---\r?\n([\s\S]*?)\r?\n---(\r?\n|$)/.exec(source)?.[1] ?? '';
}

/** `draft: true` in any spelling YAML reads as true; the plugin drops such pages. */
function isDraft(source) {
    return /^draft:[ \t]*true[ \t]*(#.*)?\r?$/im.test(frontmatter(source));
}

const COMMITTED = walkFiles(
    REPO_ROOT,
    (rel) => rel.startsWith(CONTENT_PREFIX) && /\.mdx?$/.test(rel),
).map((rel) => ({
    rel,
    id: pageId(rel),
    source: readFileSync(`${REPO_ROOT}${rel}`, 'utf8'),
}));

/** Every page id the build knows, drafts included. */
const ALL_IDS = [
    ...new Set([
        ...COMMITTED.map((page) => page.id),
        ...sidebarSlugs(referenceSidebar()),
    ]),
];

/** The pages the plugin publishes: it drops drafts. Generated pages are never drafts. */
const PUBLISHED_IDS = [
    ...new Set([
        ...COMMITTED.filter((page) => !isDraft(page.source)).map(
            (page) => page.id,
        ),
        ...sidebarSlugs(referenceSidebar()),
    ]),
];

describe('frontmatter helpers', () => {
    it.each([
        ['---\ndraft: true\n---\n', true],
        ['---\ndraft: True\n---\n', true],
        ['---\ndraft: false\n---\n', false],
        ['---\ntitle: a\n---\ndraft: true\n', false],
    ])('isDraft(%j) -> %s', (source, expected) => {
        expect(isDraft(source)).toBe(expected);
    });

    it('reads slug only from the frontmatter', () => {
        expect(frontmatter('---\ntitle: a\n---\n```yaml\nslug: x\n```\n')).toBe(
            'title: a',
        );
    });
});

describe('page inventory', () => {
    it('finds committed and generated pages', () => {
        expect(PUBLISHED_IDS).toContain('index');
        expect(PUBLISHED_IDS).toContain('guides/self-hosting');
        expect(PUBLISHED_IDS).toContain('design/cryptography');
        expect(PUBLISHED_IDS).toContain('reference/cli/commands');
    });

    // pageId mirrors the file path; a `slug:` override would make it lie.
    it('has no page overriding its id with a slug', () => {
        const overridden = COMMITTED.filter((page) =>
            /^slug:/m.test(frontmatter(page.source)),
        ).map((page) => page.rel);
        expect(overridden).toEqual([]);
    });
});

describe('LLMS_SETS', () => {
    it('has unique labels', () => {
        const labels = LLMS_SETS.map((set) => set.label);
        expect(new Set(labels).size).toBe(labels.length);
    });

    it.each(LLMS_SETS.map((set) => [set.label, set]))(
        '%s has a description and paths',
        (_, set) => {
            expect(set.description.trim()).not.toBe('');
            expect(set.paths.length).toBeGreaterThan(0);
        },
    );

    // A renamed, moved or drafted page would otherwise drop out of its bundle unnoticed.
    it.each(
        LLMS_SETS.flatMap((set) => set.paths.map((glob) => [set.label, glob])),
    )('%s: %s matches a published page', (_, glob) => {
        expect(micromatch(PUBLISHED_IDS, glob)).not.toEqual([]);
    });

    it('puts the self-hosting guide in the Self-Hosting set', () => {
        const set = LLMS_SETS.find((s) => s.label === 'Self-Hosting');
        expect(micromatch(['guides/self-hosting'], set.paths)).toEqual([
            'guides/self-hosting',
        ]);
    });
});

describe('LLMS_SMALL_EXCLUDE and LLMS_PROMOTE', () => {
    // Drafts count: `blog/**` must keep excluding posts once they publish.
    it.each([...LLMS_SMALL_EXCLUDE, ...LLMS_PROMOTE])(
        '%s matches a page',
        (glob) => {
            expect(micromatch(ALL_IDS, glob)).not.toEqual([]);
        },
    );

    it('keeps the guides in llms-small', () => {
        expect(micromatch(['guides/self-hosting'], LLMS_SMALL_EXCLUDE)).toEqual(
            [],
        );
    });
});

describe('LLMS_DETAILS', () => {
    it('carries no headings, which llms.txt reserves for its sections', () => {
        expect(LLMS_DETAILS).not.toMatch(/^#/m);
    });
});
