// Every authored docs page must carry an audit pin (see src/content.config.ts
// and Core Principles — Audit Status): `audit: none`, or a stamp written by
// scripts/audit-page.mjs. No generated page may: a generated page tracks its
// artifact and has no audit state. A splash page has no sidebar to badge, so
// it may leave the field out.
//
// The content schema cannot require the field itself: plugin routes such as
// the blog index render through <StarlightPage> with no frontmatter of their
// own, and a required field would reject them. So the schema leaves it
// optional and this test is what keeps an authored page from landing unmarked.
import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
    BLOG_AUTHORS,
    isDraftPost,
    postContractViolations,
} from '../src/lib/blog-posts.mjs';
import {
    auditDigest,
    auditStatus,
    parseAudit,
} from '../src/lib/page-audit.mjs';
import { frontmatterGenerated, frontmatterTemplate } from './lib/markdown.mjs';

const DOCS = join(
    dirname(fileURLToPath(import.meta.url)),
    '..',
    'src/content/docs',
);

/** Run git in the docs directory, or return undefined when it fails. */
function git(args) {
    try {
        return execFileSync('git', args, {
            cwd: DOCS,
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'ignore'],
        });
    } catch {
        return undefined;
    }
}

describe('frontmatterTemplate', () => {
    it.each([
        ['---\ntitle: A\ntemplate: splash\n---\nbody', 'splash'],
        ["---\ntemplate: 'doc'\n---\n", 'doc'],
        ['---\r\ntemplate: splash\r\n---\r\n', 'splash'],
        ['---\ntitle: A\n---\ntemplate: splash\n', undefined],
        ['no frontmatter\ntemplate: splash\n', undefined],
    ])('%j -> %s', (source, expected) => {
        expect(frontmatterTemplate(source)).toBe(expected);
    });
});

describe('frontmatterGenerated', () => {
    it.each([
        ['---\ntitle: A\ngenerated:\n  surface: CLI\n---\n', true],
        ['---\ntitle: A\naudit: none\n---\n', false],
        ['---\ntitle: A\n---\ngenerated:\n', false],
        ['no frontmatter', false],
    ])('%j -> %s', (source, expected) => {
        expect(frontmatterGenerated(source)).toBe(expected);
    });
});

describe('docs pages', () => {
    // Every page on disk. Generated reference pages exist only after
    // `gen-reference` has run, so they are checked here when present; that
    // they carry no audit is also asserted by gen-reference.test.mjs.
    const pages = readdirSync(DOCS, { recursive: true, encoding: 'utf8' })
        .map((rel) => rel.split('\\').join('/'))
        .filter((rel) => /\.mdx?$/.test(rel))
        .map((rel) => ({ rel, source: readFileSync(join(DOCS, rel), 'utf8') }));

    it('finds the authored pages', () => {
        expect(pages.length).toBeGreaterThan(10);
    });

    it('each authored page declares audit: none or a valid stamp', () => {
        const unmarked = pages
            .filter(({ source }) => !frontmatterGenerated(source))
            .map(({ rel, source }) => ({
                rel,
                audit: parseAudit(source),
                splash: frontmatterTemplate(source) === 'splash',
            }))
            .filter(
                ({ audit, splash }) =>
                    audit.state === 'malformed' ||
                    (audit.state === 'absent' && !splash),
            )
            .map(({ rel, audit }) =>
                audit.state === 'malformed' ? `${rel}: ${audit.reason}` : rel,
            );
        expect(unmarked).toEqual([]);
    });

    it('no page still sets the retired status field', () => {
        // The schema would silently drop it; the audit pin replaced it.
        const legacy = pages
            .filter(({ source }) => /^status:/m.test(frontmatter(source)))
            .map(({ rel }) => rel);
        expect(legacy).toEqual([]);
    });

    it('no generated page declares an audit', () => {
        const claimed = pages
            .filter(
                ({ source }) =>
                    frontmatterGenerated(source) &&
                    parseAudit(source).state !== 'absent',
            )
            .map(({ rel }) => rel);
        expect(claimed).toEqual([]);
    });

    it('keeps every published blog post audited', () => {
        // A published post is reviewed as published: an edit needs a restamp.
        const unaudited = pages
            .filter(
                ({ rel, source }) =>
                    rel.startsWith('blog/') &&
                    !isDraftPost(source) &&
                    auditStatus(source) !== 'audited',
            )
            .map(({ rel }) => rel);
        expect(unaudited).toEqual([]);
    });

    it('gives every published blog post the full post contract', () => {
        // Title, a 50-160 character description, date, configured authors,
        // tags and an excerpt marker: what the index, RSS and search need.
        const incomplete = pages
            .filter(({ rel }) => rel.startsWith('blog/'))
            .map(({ rel, source }) => {
                if (isDraftPost(source)) return null;
                const violations = postContractViolations(source, BLOG_AUTHORS);
                return violations.length ? { rel, violations } : null;
            })
            .filter(Boolean);
        expect(incomplete).toEqual([]);
    });

    it('pins each stamp to a commit whose copy of the page it digests', () => {
        // Needs the stamped commit in local history; a shallow clone (the CI
        // docs job) skips each stamp it cannot reach, and says how many. A
        // page renamed since its audit has no copy at the old path to check.
        const root = git(['rev-parse', '--show-toplevel'])?.trim();
        const stamped = pages
            .map(({ rel, source }) => ({ rel, audit: parseAudit(source) }))
            .filter(({ audit }) => audit.state === 'stamp');
        const mismatched = [];
        let skipped = 0;
        for (const { rel, audit } of stamped) {
            if (
                root === undefined ||
                git(['cat-file', '-e', `${audit.commit}^{commit}`]) ===
                    undefined
            ) {
                skipped++;
                continue;
            }
            const path = relative(root, join(DOCS, rel)).split('\\').join('/');
            const then = git(['show', `${audit.commit}:${path}`]);
            if (then === undefined) {
                skipped++;
                continue;
            }
            if (auditDigest(then) !== audit.digest) mismatched.push(rel);
        }
        if (skipped > 0) {
            console.warn(
                `audit pins: ${skipped} of ${stamped.length} stamped commits not checkable here`,
            );
        }
        expect(mismatched).toEqual([]);
    });
});

/** A page's frontmatter block, or the empty string. */
function frontmatter(source) {
    return /^---\r?\n([\s\S]*?)\r?\n---(\r?\n|$)/.exec(source)?.[1] ?? '';
}
