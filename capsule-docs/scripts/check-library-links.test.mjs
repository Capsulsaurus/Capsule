import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
    checkLibraryLinks,
    compileUrls,
    handWrittenLinks,
    reportLibraryLinks,
} from './check-library-links.mjs';

let root;

function repo(files) {
    root = mkdtempSync(join(tmpdir(), 'capsule-library-links-'));
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

const LIBS = [
    { url: 'https://github.com/getkono/kynos', names: ['Kynos'] },
    { url: 'https://github.com/tokio-rs/tokio', names: ['Tokio'] },
];
const URLS = compileUrls(LIBS);
const lines = (source) =>
    handWrittenLinks(source, URLS).map(({ line, url }) => [line, url]);

describe('handWrittenLinks', () => {
    it('finds an inline link to a registered repository', () => {
        expect(
            lines(
                'One transport: [Kynos](https://github.com/getkono/kynos) REST.',
            ),
        ).toEqual([[1, 'https://github.com/getkono/kynos']]);
    });

    it('finds reference definitions, autolinks, bare URLs and HTML hrefs', () => {
        const source = [
            '[Tokio][t]',
            '[t]: https://github.com/tokio-rs/tokio',
            'See <https://github.com/getkono/kynos/>.',
            'Or https://www.github.com/getkono/kynos.git today.',
            '<a href="https://github.com/getkono/kynos#readme">Kynos</a>',
            '[Kynos](http://GitHub.com/getkono/kynos?tab=readme "title")',
        ].join('\n');
        expect(lines(source).map(([line]) => line)).toEqual([2, 3, 4, 5, 6]);
    });

    it('allows deep links into a repository and lookalike repositories', () => {
        const source = [
            '[the router](https://github.com/getkono/kynos/blob/main/src/router.rs)',
            '[issue](https://github.com/tokio-rs/tokio/issues/1)',
            '[CLI](https://github.com/getkono/kynos-cli)',
            '[fork](https://github.com/getkono/kynos.rs)',
        ].join('\n');
        expect(lines(source)).toEqual([]);
    });

    it('skips code, comments and frontmatter', () => {
        const source = [
            '---',
            'source: https://github.com/getkono/kynos',
            '---',
            'Inline `https://github.com/getkono/kynos` and ``[x](https://github.com/getkono/kynos)``.',
            '<!-- [Kynos](https://github.com/getkono/kynos) -->',
            '```md',
            '[Kynos](https://github.com/getkono/kynos)',
            '```',
            'After the fence: [Tokio](https://github.com/tokio-rs/tokio).',
        ].join('\n');
        expect(lines(source)).toEqual([
            [9, 'https://github.com/tokio-rs/tokio'],
        ]);
    });

    it('finds a bare URL that ends a sentence', () => {
        const source = [
            'See https://github.com/getkono/kynos.',
            'Or https://github.com/tokio-rs/tokio/. Then more.',
            'Not https://github.com/getkono/kynos.rs here.',
        ].join('\n');
        expect(lines(source)).toEqual([
            [1, 'https://github.com/getkono/kynos'],
            [2, 'https://github.com/tokio-rs/tokio'],
        ]);
    });

    it('skips indented code blocks but not list continuations', () => {
        const source = [
            'Prose.',
            '',
            '    https://github.com/getkono/kynos',
            '',
            '\thttps://github.com/tokio-rs/tokio',
            'A paragraph',
            '    https://github.com/getkono/kynos lazily continued.',
            '',
            '- An item',
            '',
            '    https://github.com/tokio-rs/tokio continues the item.',
            '',
            '        https://github.com/getkono/kynos',
            '',
            'Back at the margin: https://github.com/tokio-rs/tokio',
        ].join('\n');
        expect(lines(source)).toEqual([
            [7, 'https://github.com/getkono/kynos'],
            [11, 'https://github.com/tokio-rs/tokio'],
            [15, 'https://github.com/tokio-rs/tokio'],
        ]);
    });

    it('reads indented lines as prose in MDX', () => {
        expect(
            handWrittenLinks(
                'Prose.\n\n    https://github.com/getkono/kynos',
                URLS,
                {
                    indentedCode: false,
                },
            ),
        ).toEqual([{ line: 3, url: 'https://github.com/getkono/kynos' }]);
    });

    it('reports each occurrence on a line', () => {
        expect(
            lines(
                '[a](https://github.com/getkono/kynos) and [b](https://github.com/getkono/kynos)',
            ),
        ).toHaveLength(2);
    });
});

describe('checkLibraryLinks', () => {
    it('scans site pages only and names file, line and URL', () => {
        const r = repo({
            'capsule-docs/src/content/docs/design/api.md':
                '# API\n\n[Kynos](https://github.com/getkono/kynos) REST.\n',
            'capsule-docs/src/content/docs/blog/post.mdx':
                'Built on [Tokio](https://github.com/tokio-rs/tokio).\n',
            'capsule-docs/src/content/docs/guides/ok.md': 'Kynos REST.\n',
            'README.md': '[Kynos](https://github.com/getkono/kynos)\n',
        });
        const result = checkLibraryLinks(r, LIBS);
        expect(result.checked).toBe(3);
        expect(result.findings).toEqual([
            "capsule-docs/src/content/docs/blog/post.mdx:1  https://github.com/tokio-rs/tokio  write the library's name; the build links it",
            "capsule-docs/src/content/docs/design/api.md:3  https://github.com/getkono/kynos  write the library's name; the build links it",
        ]);
    });

    it('passes a tree that names libraries without linking them', () => {
        const r = repo({
            'capsule-docs/src/content/docs/design/api.md':
                'Kynos and `tokio`.\n',
        });
        expect(checkLibraryLinks(r, LIBS).findings).toEqual([]);
    });
});

describe('reportLibraryLinks', () => {
    it('summarises a pass and lists failures', () => {
        expect(reportLibraryLinks({ findings: [], checked: 4 })).toContain(
            '4 page(s), no hand-written link',
        );
        const failed = reportLibraryLinks({
            findings: ['a.md:1  x'],
            checked: 4,
        });
        expect(failed).toContain('  a.md:1  x');
        expect(failed).toContain('library-links failed: 1 finding(s)');
    });
});
