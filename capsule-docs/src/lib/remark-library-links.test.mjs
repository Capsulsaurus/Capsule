import { fromMarkdown } from 'mdast-util-from-markdown';
import { toString as textOf } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';
import { describe, expect, it } from 'vitest';
import { EXPLICIT_ONLY, LIBRARIES } from './library-links.mjs';
import {
    BADGE_CLASS,
    BADGE_TEXT,
    forbiddenNames,
    linkLibraries,
} from './remark-library-links.mjs';

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

/** True for a repo badge the plugin inserted. */
const isBadge = (node) =>
    node.type === 'link' &&
    (node.data?.hProperties?.className ?? []).includes(BADGE_CLASS);

/**
 * A document's text as a reader sees it, with each repo badge written inline
 * as `{url}` and any other link as `[text](url)`; blocks on their own lines.
 */
function render(markdown, libraries) {
    const tree = linkLibraries(fromMarkdown(markdown), libraries);
    const inline = (node) => {
        if (isBadge(node)) return `{${node.url}}`;
        if (node.type === 'link') return `[${textOf(node)}](${node.url})`;
        if ('value' in node) return node.value;
        return (node.children ?? []).map(inline).join('');
    };
    return tree.children.map(inline).join('\n');
}

/** Every repo badge in a document. */
function badges(markdown, libraries) {
    const tree = linkLibraries(fromMarkdown(markdown), libraries);
    const out = [];
    visit(tree, 'link', (node) => {
        if (isBadge(node)) out.push(node);
    });
    return out;
}

describe('linkLibraries', () => {
    it('badges only the first mention on a page', () => {
        expect(
            render('Kynos routes. Kynos emits.\n\nKynos again.', [KYNOS]),
        ).toBe(`Kynos {${KYNOS.url}} routes. Kynos emits.\nKynos again.`);
    });

    it('leaves the name itself plain text, not a link', () => {
        const tree = linkLibraries(fromMarkdown('Built on Kynos today.'), [
            KYNOS,
        ]);
        const [paragraph] = tree.children;
        expect(paragraph.children.map((node) => node.type)).toEqual([
            'text',
            'text',
            'text',
            'link',
            'text',
        ]);
        expect(paragraph.children[1]).toEqual({ type: 'text', value: 'Kynos' });
        // A real space, so the name and the badge stay two words in search
        // text and copied text.
        expect(paragraph.children[2]).toEqual({ type: 'text', value: ' ' });
    });

    it('makes the badge a labelled link to the source repository', () => {
        const [badge] = badges('Built on Kynos.', [KYNOS]);
        expect(badge.url).toBe(KYNOS.url);
        expect(badge.title).toBe('Source repository: github.com/getkono/kynos');
        expect(badge.data.hProperties).toEqual({
            className: [BADGE_CLASS],
            // Contains the visible label, so speech control can name it.
            ariaLabel: `Kynos source ${BADGE_TEXT}`,
            // The badge is chrome, not content: search never indexes it.
            dataPagefindIgnore: '',
        });
        expect(textOf(badge)).toBe(BADGE_TEXT);
    });

    it('badges a mention inside a table cell', () => {
        expect(
            render('| a | b |\n| - | - |\n| x | the Kynos router |', [KYNOS]),
        ).toContain(`the Kynos {${KYNOS.url}} router`);
    });

    it('skips headings, existing links and code blocks', () => {
        expect(
            render(
                '## Kynos\n\n[the Kynos docs](https://x.invalid)\n\n```\nKynos\n```\n\nThen Kynos.',
                [KYNOS],
            ),
        ).toBe(
            `Kynos\n[the Kynos docs](https://x.invalid)\nKynos\nThen Kynos {${KYNOS.url}}.`,
        );
    });

    it('matches whole words only', () => {
        expect(
            badges('Kynoses, kynos, Kynos-like, a/Kynos, Kynos.toml.', [KYNOS]),
        ).toEqual([]);
        expect(render('Ends with Kynos.', [KYNOS])).toBe(
            `Ends with Kynos {${KYNOS.url}}.`,
        );
    });

    it('prefers the longest name', () => {
        expect(render('Uses TanStack Router.', TANSTACK)).toBe(
            `Uses TanStack Router {${TANSTACK[0].url}}.`,
        );
    });

    it('badges an exact inline-code spelling, and it counts as the mention', () => {
        expect(
            render('`rustls` first, then rustls, and `rustls-tls`.', [RUSTLS]),
        ).toBe(`rustls {${RUSTLS.url}} first, then rustls, and rustls-tls.`);
    });

    it('keeps an inline-code mention as code', () => {
        const tree = linkLibraries(fromMarkdown('Uses `rustls`.'), [RUSTLS]);
        const [paragraph] = tree.children;
        expect(paragraph.children[1]).toEqual(
            expect.objectContaining({ type: 'inlineCode', value: 'rustls' }),
        );
        expect(isBadge(paragraph.children[3])).toBe(true);
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
