// Every authored docs page must carry a review `status` (see
// src/content.config.ts and Core Principles — Review Status).
//
// The content schema cannot require the field itself: plugin routes such as
// the blog index render through <StarlightPage> with no frontmatter of their
// own, and a required field would reject them. So the schema leaves it
// optional and this test is what keeps an authored page from landing unmarked.
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { frontmatterStatus } from './lib/markdown.mjs';

const DOCS = join(
    dirname(fileURLToPath(import.meta.url)),
    '..',
    'src/content/docs',
);

describe('frontmatterStatus', () => {
    it.each([
        ['---\ntitle: A\nstatus: draft\n---\nbody', 'draft'],
        ["---\nstatus: 'stable'\n---\n", 'stable'],
        ['---\nstatus: draft # pending review\n---\n', 'draft'],
        ['---\r\ntitle: A\r\nstatus: stable\r\n---\r\nbody', 'stable'],
        ['---\ntitle: A\n---\nstatus: draft\n', undefined],
        ['no frontmatter\nstatus: draft\n', undefined],
    ])('%j -> %s', (source, expected) => {
        expect(frontmatterStatus(source)).toBe(expected);
    });
});

describe('docs pages', () => {
    // Every page on disk. Generated reference pages exist only after
    // `gen-reference` has run, so they are checked here when present; their
    // status is guaranteed by the renderers' own tests in gen-reference.test.mjs.
    const pages = readdirSync(DOCS, { recursive: true, encoding: 'utf8' })
        .map((rel) => rel.split('\\').join('/'))
        .filter((rel) => /\.mdx?$/.test(rel));

    it('finds the authored pages', () => {
        expect(pages.length).toBeGreaterThan(10);
    });

    it('each declares status: draft or stable', () => {
        const unmarked = pages.filter((rel) => {
            const status = frontmatterStatus(
                readFileSync(join(DOCS, rel), 'utf8'),
            );
            return status !== 'draft' && status !== 'stable';
        });
        expect(unmarked).toEqual([]);
    });
});
