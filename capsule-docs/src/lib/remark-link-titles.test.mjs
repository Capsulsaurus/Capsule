import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString as textOf } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import remarkLinkTitles, {
    frontmatterTitle,
    isGeneratedPage,
    labelFor,
    labelParts,
    localizeUrl,
    mapAnchor,
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

    it('splits a label into the page and the heading it names', () => {
        expect(labelParts('/design/metadata/', outlineFor, here)).toEqual({
            page: 'Metadata',
            heading: null,
        });
        expect(
            labelParts('/design/metadata/#identifiers', outlineFor, here),
        ).toEqual({ page: 'Metadata', heading: 'Identifiers' });
        expect(labelParts('#local-heading', outlineFor, here)).toEqual({
            page: null,
            heading: 'Local Heading',
        });
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

    /** Run the plugin over a page; its tree. */
    function run(markdown, frontmatter = { title: 'Page' }) {
        const tree = fromMarkdown(markdown);
        remarkLinkTitles({ contentDir })(tree, {
            path: 'page.md',
            value: markdown,
            data: { astro: { frontmatter } },
        });
        return tree;
    }

    function labels(markdown, frontmatter) {
        const out = [];
        visit(run(markdown, frontmatter), 'link', (node) => {
            out.push([node.url, textOf(node)]);
        });
        return out;
    }

    /** Each link's class and its children as [class, text] pairs. */
    function parts(markdown) {
        const out = [];
        visit(run(markdown), 'link', (node) => {
            out.push({
                className: node.data?.hProperties?.className,
                children: node.children.map((child) => [
                    child.data?.hProperties?.className?.[0],
                    child.value,
                ]),
            });
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
                    '[](https://example.com/) [the spec](https://example.com/spec)',
                    '[rustdoc](/rustdoc/capsule_sdk/)',
                ].join('\n'),
            ),
        ).toEqual([
            ['/design/cryptography/', 'Cryptography'],
            [
                '/design/metadata/#identifiers',
                `Metadata${SEPARATOR}Identifiers`,
            ],
            ['#local', 'Local'],
            ['https://example.com/', ''],
            ['https://example.com/spec', 'the spec'],
            ['/rustdoc/capsule_sdk/', 'rustdoc'],
        ]);
    });

    it('marks each label part so the page and the heading can be styled apart', () => {
        expect(
            parts(
                '## Local\n\n[](/design/metadata/) [](/design/metadata/#identifiers) [](#local)',
            ),
        ).toEqual([
            {
                className: ['xref'],
                children: [['xref-page', 'Metadata']],
            },
            {
                className: ['xref'],
                children: [
                    ['xref-page', 'Metadata'],
                    ['xref-sep', SEPARATOR],
                    ['xref-heading', 'Identifiers'],
                ],
            },
            {
                className: ['xref'],
                children: [['xref-heading', 'Local']],
            },
        ]);
    });

    it('fails the build naming the page and the link', () => {
        expect(() => labels('[](/design/metadata/#nope)')).toThrow(
            /page\.md: empty link \[\]\(\/design\/metadata\/#nope\)/,
        );
        expect(() => labels('[](/design/metadata/#100%)')).toThrow(
            /page\.md: empty link .* #100% is not a valid percent-encoded fragment/,
        );
    });

    it('keeps a link title and any class an earlier plugin set', () => {
        const tree = fromMarkdown('[](/design/metadata/ "Hover")');
        visit(tree, 'link', (node) => {
            node.data = { hProperties: { className: 'earlier' } };
        });
        remarkLinkTitles({ contentDir })(tree, {
            path: 'page.md',
            data: { astro: { frontmatter: { title: 'Page' } } },
        });
        const links = [];
        visit(tree, 'link', (node) => {
            links.push(node);
        });
        expect(links[0].title).toBe('Hover');
        expect(links[0].data.hProperties.className).toEqual([
            'earlier',
            'xref',
        ]);
    });

    it('rejects a hand-written label on a link to a docs page or heading', () => {
        expect(() => labels('[the metadata doc](/design/metadata/)')).toThrow(
            /page\.md: \[the metadata doc\]\(\/design\/metadata\/\) has its own text; write it \[\]\(\/design\/metadata\/\)/,
        );
        expect(() => labels('## Local\n\n[up there](#local)')).toThrow(
            /\[up there\]\(#local\) has its own text/,
        );
    });

    it('allows a hand-written label on a generated page', () => {
        expect(
            labels('[`Asset`](/design/metadata/)', {
                title: 'Swift',
                generated: { surface: 'Swift SDK', source: 'x.json' },
            }),
        ).toEqual([['/design/metadata/', 'Asset']]);
    });
});

describe('isGeneratedPage', () => {
    it('reads the generated marker from Astro frontmatter or the source', () => {
        expect(
            isGeneratedPage({
                data: {
                    astro: { frontmatter: { generated: { surface: 'x' } } },
                },
            }),
        ).toBe(true);
        expect(
            isGeneratedPage({
                value: '---\ntitle: X\ngenerated:\n  surface: "X"\n---\n',
            }),
        ).toBe(true);
        expect(
            isGeneratedPage({ value: '---\ntitle: X\n---\n\ngenerated: no\n' }),
        ).toBe(false);
    });
});

describe('mapAnchor', () => {
    const english = outlineOf(
        '---\ntitle: E\n---\n## Setup\n### Keys\n## Use\n',
    );
    const alike = outlineOf('---\ntitle: J\n---\n## 設定\n### 鍵\n## 使い方\n');
    const unlike = outlineOf('---\ntitle: J\n---\n## 設定\n## 使い方\n');

    it('maps a heading to the one in the same place of a page laid out alike', () => {
        expect(mapAnchor('keys', english, alike)).toBe('鍵');
        expect(mapAnchor('use', english, alike)).toBe('使い方');
    });

    it('maps nothing when the layouts differ or the heading is not there', () => {
        expect(mapAnchor('keys', english, unlike)).toBeNull();
        expect(mapAnchor('gone', english, alike)).toBeNull();
    });
});

describe('localizeUrl', () => {
    const pages = {
        'guides/self-hosting/': outlineOf(
            '---\ntitle: Self Hosting\n---\n## Configure\n## Upgrade\n',
        ),
        'ja/guides/self-hosting/': outlineOf(
            '---\ntitle: セルフホスティング\n---\n## 設定\n## 更新\n',
        ),
        'ko/guides/self-hosting/': outlineOf(
            '---\ntitle: 셀프 호스팅\n---\n## 설정\n',
        ),
        'design/keys/': outlineOf('---\ntitle: Keys\n---\n## Rotation\n'),
    };
    const ownOutlineFor = (route) =>
        pages[route.replace(/^\/+/, '').replace(/\/?$/, '/')] ?? null;
    const here = outlineOf('---\ntitle: ここ\n---\n## 概要\n');
    const original = outlineOf('---\ntitle: Here\n---\n## Overview\n');
    const context = (locale) => ({ locale, ownOutlineFor, here, original });

    it.each([
        // English pages link as written.
        [
            undefined,
            '/guides/self-hosting/#configure',
            '/guides/self-hosting/#configure',
        ],
        // A translated page: its translation, the anchor mapped by place.
        ['ja', '/guides/self-hosting/', '/ja/guides/self-hosting/'],
        [
            'ja',
            '/guides/self-hosting/#upgrade',
            '/ja/guides/self-hosting/#更新',
        ],
        [
            'ja',
            '/ja/guides/self-hosting/#configure',
            '/ja/guides/self-hosting/#設定',
        ],
        ['ja', '/guides/self-hosting/#設定', '/ja/guides/self-hosting/#設定'],
        // An untranslated page: the locale's fallback route, English anchors.
        ['ja', '/design/keys/#rotation', '/ja/design/keys/#rotation'],
        // Laid out differently: the English page, which has the heading.
        [
            'ko',
            '/guides/self-hosting/#upgrade',
            '/guides/self-hosting/#upgrade',
        ],
        [
            'ko',
            '/ko/guides/self-hosting/#upgrade',
            '/guides/self-hosting/#upgrade',
        ],
        // Same page: an English anchor maps to this page's heading.
        ['ja', '#overview', '#概要'],
        ['ja', '#概要', '#概要'],
        // Not a docs page, another locale, or a broken link: as written.
        ['ja', '/rustdoc/capsule_sdk/', '/rustdoc/capsule_sdk/'],
        ['ja', '/ko/guides/self-hosting/', '/ko/guides/self-hosting/'],
        ['ja', '/gone/', '/gone/'],
        ['ko', '/guides/self-hosting/#gone', '/guides/self-hosting/#gone'],
        ['ja', '#gone', '#gone'],
    ])('on a %s page, %s -> %s', (locale, url, expected) => {
        expect(localizeUrl(url, context(locale))).toBe(expected);
    });
});

describe('remarkLinkTitles on a translated page', () => {
    let contentDir;

    beforeEach(() => {
        contentDir = mkdtempSync(join(tmpdir(), 'link-titles-locale-'));
        const files = {
            'guides/start.md':
                '---\ntitle: Getting Started\n---\n\n## Install\n',
            'ja/guides/start.md':
                '---\ntitle: はじめに\n---\n\n## インストール\n',
            'guides/user.md': '---\ntitle: User Guide\n---\n\n## Import\n',
            'ja/guides/user.md':
                '---\ntitle: ユーザーガイド\n---\n\n## インポート\n',
            'design/keys.md': '---\ntitle: Keys\n---\n\n## Rotation\n',
        };
        for (const [rel, body] of Object.entries(files)) {
            mkdirSync(join(contentDir, rel, '..'), { recursive: true });
            writeFileSync(join(contentDir, rel), body);
        }
    });

    afterEach(() => rmSync(contentDir, { recursive: true, force: true }));

    function labels(rel, markdown) {
        const tree = fromMarkdown(markdown);
        remarkLinkTitles({ contentDir })(tree, {
            path: join(contentDir, rel),
            value: markdown,
            data: { astro: { frontmatter: { title: 'T' } } },
        });
        const out = [];
        visit(tree, 'link', (node) => {
            out.push([node.url, textOf(node)]);
        });
        return out;
    }

    it('stays in the locale, naming the translation or the English fallback', () => {
        expect(
            labels(
                'ja/guides/user.md',
                '## インポート\n\n[](/guides/start/#install) [](/design/keys/#rotation) [](#import)',
            ),
        ).toEqual([
            [
                '/ja/guides/start/#インストール',
                `はじめに${SEPARATOR}インストール`,
            ],
            ['/ja/design/keys/#rotation', `Keys${SEPARATOR}Rotation`],
            ['#インポート', 'インポート'],
        ]);
    });

    it('leaves English pages linking English', () => {
        expect(labels('guides/user.md', '[](/guides/start/)')).toEqual([
            ['/guides/start/', 'Getting Started'],
        ]);
    });

    it('still fails on a link nothing serves, naming it as written', () => {
        expect(() =>
            labels('ja/guides/user.md', '[](/design/keys/#gone)'),
        ).toThrow(
            /empty link \[\]\(\/design\/keys\/#gone\) cannot be labelled: \/ja\/design\/keys\/ has no heading #gone/,
        );
    });
});
