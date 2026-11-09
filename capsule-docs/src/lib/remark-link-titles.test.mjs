import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString as textOf } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import remarkLinkTitles, {
    frontmatterTitle,
    labelFor,
    outlineOf,
    SEPARATOR,
} from './remark-link-titles.mjs';

describe('frontmatterTitle', () => {
    it('reads bare, double-quoted and single-quoted titles', () => {
        expect(frontmatterTitle('---\ntitle: Metadata\n---\n')).toBe(
            'Metadata',
        );
        expect(frontmatterTitle('---\ntitle: "A: B"\n---\n')).toBe('A: B');
        expect(frontmatterTitle("---\ntitle: 'It''s'\n---\n")).toBe("It's");
        expect(frontmatterTitle('# No frontmatter\n')).toBeNull();
    });
});

describe('outlineOf', () => {
    it('slugs headings as Astro does, suffixing repeats', () => {
        const outline = outlineOf(
            '---\ntitle: Keys\nstatus: draft\n---\n\n## Device Keys\n\n### `amk_version` rules\n\n## Device Keys\n',
        );
        expect(outline.title).toBe('Keys');
        expect([...outline.headings]).toEqual([
            ['device-keys', 'Device Keys'],
            ['amk_version-rules', 'amk_version rules'],
            ['device-keys-1', 'Device Keys'],
        ]);
    });

    it('does not read the frontmatter fence as a heading', () => {
        expect(
            [...outlineOf('---\ntitle: T\n---\n\nBody.\n').headings].length,
        ).toBe(0);
    });
});

describe('labelFor', () => {
    const pages = {
        '/design/metadata/': outlineOf(
            '---\ntitle: Metadata\n---\n\n## Identifiers\n',
        ),
    };
    const outlineFor = (route) => pages[route] ?? null;
    const here = outlineOf('---\ntitle: Here\n---\n\n## Local Heading\n');

    it('renders a page title, a cross-page heading and a same-page heading', () => {
        expect(labelFor('/design/metadata/', outlineFor, here)).toBe(
            'Metadata',
        );
        expect(
            labelFor('/design/metadata/#identifiers', outlineFor, here),
        ).toBe(`Metadata${SEPARATOR}Identifiers`);
        expect(labelFor('#local-heading', outlineFor, here)).toBe(
            'Local Heading',
        );
    });

    it('fails on a missing page or anchor', () => {
        expect(() => labelFor('/nope/', outlineFor, here)).toThrow(
            /no page serves \/nope\//,
        );
        expect(() =>
            labelFor('/design/metadata/#gone', outlineFor, here),
        ).toThrow(/no heading #gone/);
        expect(() => labelFor('#gone', outlineFor, here)).toThrow(
            /this page has no heading #gone/,
        );
    });
});

describe('remarkLinkTitles', () => {
    let contentDir;

    beforeEach(() => {
        contentDir = mkdtempSync(join(tmpdir(), 'link-titles-'));
        mkdirSync(join(contentDir, 'design/cryptography'), {
            recursive: true,
        });
        writeFileSync(
            join(contentDir, 'design/cryptography/index.md'),
            '---\ntitle: Cryptography\n---\n\n## Trust Boundary\n',
        );
        writeFileSync(
            join(contentDir, 'design/metadata.md'),
            '---\ntitle: Metadata\n---\n\n## Identifiers\n',
        );
    });

    afterEach(() => rmSync(contentDir, { recursive: true, force: true }));

    function labels(markdown) {
        const tree = fromMarkdown(markdown);
        remarkLinkTitles({ contentDir })(tree, {
            path: 'page.md',
            value: markdown,
        });
        const out = [];
        visit(tree, 'link', (node) => {
            out.push([node.url, textOf(node)]);
        });
        return out;
    }

    it('labels empty site links from their targets and leaves the rest alone', () => {
        expect(
            labels(
                [
                    '## Local',
                    '',
                    '[](/design/cryptography/) [](/design/metadata/#identifiers) [](#local)',
                    '[kept](/design/metadata/) [](https://example.com/)',
                ].join('\n'),
            ),
        ).toEqual([
            ['/design/cryptography/', 'Cryptography'],
            [
                '/design/metadata/#identifiers',
                `Metadata${SEPARATOR}Identifiers`,
            ],
            ['#local', 'Local'],
            ['/design/metadata/', 'kept'],
            ['https://example.com/', ''],
        ]);
    });

    it('fails the build naming the page and the link', () => {
        expect(() => labels('[](/design/metadata/#nope)')).toThrow(
            /page\.md: empty link \[\]\(\/design\/metadata\/#nope\)/,
        );
    });
});
