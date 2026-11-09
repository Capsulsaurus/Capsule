// Every authored docs page must carry a review `status` (see
// src/content.config.ts and Core Principles — Review Status), and no generated
// page may: a generated page tracks its artifact and has no review state.
//
// The content schema cannot require the field itself: plugin routes such as
// the blog index render through <StarlightPage> with no frontmatter of their
// own, and a required field would reject them. So the schema leaves it
// optional and this test is what keeps an authored page from landing unmarked.
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
    BLOG_AUTHORS,
    isDraftPost,
    postContractViolations,
} from '../src/lib/blog-posts.mjs';
import { frontmatterGenerated, frontmatterStatus } from './lib/markdown.mjs';

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

describe('frontmatterGenerated', () => {
    it.each([
        ['---\ntitle: A\ngenerated:\n  surface: CLI\n---\n', true],
        ['---\ntitle: A\nstatus: draft\n---\n', false],
        ['---\ntitle: A\n---\ngenerated:\n', false],
        ['no frontmatter', false],
    ])('%j -> %s', (source, expected) => {
        expect(frontmatterGenerated(source)).toBe(expected);
    });
});

describe('docs pages', () => {
    // Every page on disk. Generated reference pages exist only after
    // `gen-reference` has run, so they are checked here when present; that
    // they carry no status is also asserted by gen-reference.test.mjs.
    const pages = readdirSync(DOCS, { recursive: true, encoding: 'utf8' })
        .map((rel) => rel.split('\\').join('/'))
        .filter((rel) => /\.mdx?$/.test(rel));

    it('finds the authored pages', () => {
        expect(pages.length).toBeGreaterThan(10);
    });

    it('each authored page declares status: draft or stable', () => {
        const unmarked = pages.filter((rel) => {
            const source = readFileSync(join(DOCS, rel), 'utf8');
            if (frontmatterGenerated(source)) return false;
            const status = frontmatterStatus(source);
            return status !== 'draft' && status !== 'stable';
        });
        expect(unmarked).toEqual([]);
    });

    it('no generated page declares a status', () => {
        const claimed = pages.filter((rel) => {
            const source = readFileSync(join(DOCS, rel), 'utf8');
            return (
                frontmatterGenerated(source) &&
                frontmatterStatus(source) !== undefined
            );
        });
        expect(claimed).toEqual([]);
    });

    it('marks every published blog post stable', () => {
        // A published post is reviewed: it must never show a Draft badge.
        const unreviewed = pages.filter((rel) => {
            if (!rel.startsWith('blog/')) return false;
            const source = readFileSync(join(DOCS, rel), 'utf8');
            return (
                !isDraftPost(source) && frontmatterStatus(source) !== 'stable'
            );
        });
        expect(unreviewed).toEqual([]);
    });

    it('gives every published blog post the full post contract', () => {
        // Title, a 50-160 character description, date, configured authors,
        // tags and an excerpt marker: what the index, RSS and search need.
        const incomplete = pages
            .filter((rel) => rel.startsWith('blog/'))
            .map((rel) => {
                const source = readFileSync(join(DOCS, rel), 'utf8');
                if (isDraftPost(source)) return null;
                const violations = postContractViolations(source, BLOG_AUTHORS);
                return violations.length ? { rel, violations } : null;
            })
            .filter(Boolean);
        expect(incomplete).toEqual([]);
    });
});
