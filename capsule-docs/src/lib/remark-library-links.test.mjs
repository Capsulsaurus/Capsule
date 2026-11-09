import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';
import { describe, expect, it } from 'vitest';
import { EXPLICIT_ONLY, LIBRARIES } from './library-links.mjs';
import { forbiddenNames, linkLibraries } from './remark-library-links.mjs';

const KYNOS = { url: 'https://github.com/getkono/kynos', names: ['Kynos'] };
const TANSTACK = [
    { url: 'https://github.com/TanStack/router', names: ['TanStack Router'] },
    { url: 'https://example.invalid/tanstack', names: ['TanStack'] },
];
const RUSTLS = {
    url: 'https://github.com/rustls/rustls',
    names: ['rustls'],
    code: ['rustls'],
};

/** Every link in a document: [text, url]. */
function links(markdown, libraries) {
    const tree = linkLibraries(fromMarkdown(markdown), libraries);
    const out = [];
    visit(tree, 'link', (node) => {
        out.push([toString(node), node.url]);
    });
    return out;
}

describe('linkLibraries', () => {
    it('links only the first mention on a page', () => {
        expect(
            links('Kynos routes. Kynos emits.\n\nKynos again.', [KYNOS]),
        ).toEqual([['Kynos', KYNOS.url]]);
    });

    it('keeps the text around a mention', () => {
        const tree = linkLibraries(fromMarkdown('Built on Kynos today.'), [
            KYNOS,
        ]);
        expect(toString(tree)).toBe('Built on Kynos today.');
    });

    it('skips headings, existing links and code blocks', () => {
        expect(
            links(
                '## Kynos\n\n[the Kynos docs](https://x.invalid)\n\n```\nKynos\n```\n\nThen Kynos.',
                [KYNOS],
            ),
        ).toEqual([
            ['the Kynos docs', 'https://x.invalid'],
            ['Kynos', KYNOS.url],
        ]);
    });

    it('matches whole words only', () => {
        expect(
            links('Kynoses, kynos, Kynos-like, a/Kynos, Kynos.toml.', [KYNOS]),
        ).toEqual([]);
        expect(links('Ends with Kynos.', [KYNOS])).toEqual([
            ['Kynos', KYNOS.url],
        ]);
    });

    it('prefers the longest name', () => {
        expect(links('Uses TanStack Router.', TANSTACK)).toEqual([
            ['TanStack Router', TANSTACK[0].url],
        ]);
    });

    it('links an exact inline-code spelling, and it counts as the mention', () => {
        expect(
            links('`rustls` first, then rustls, and `rustls-tls`.', [RUSTLS]),
        ).toEqual([['rustls', RUSTLS.url]]);
    });
});

describe('the library map', () => {
    it('never auto-links an explicit-only name', () => {
        expect(forbiddenNames()).toEqual([]);
        expect(Object.keys(EXPLICIT_ONLY)).toContain('gamut');
    });

    it('points every library at an https source repository, once', () => {
        const urls = LIBRARIES.map((library) => library.url);
        for (const url of urls) {
            expect(url).toMatch(/^https:\/\/[^/]+\/[\w.-]+\/[\w.-]+$/);
        }
        expect(new Set(urls).size).toBe(urls.length);
    });

    it('gives every spelling to exactly one library', () => {
        const spellings = LIBRARIES.flatMap((library) => [
            ...library.names,
            ...(library.code ?? []),
        ]);
        const names = LIBRARIES.flatMap((library) => library.names);
        const code = LIBRARIES.flatMap((library) => library.code ?? []);
        expect(new Set(names).size).toBe(names.length);
        expect(new Set(code).size).toBe(code.length);
        expect(spellings.length).toBeGreaterThan(0);
    });
});
