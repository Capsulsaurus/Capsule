import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
    BLOG_AUTHORS,
    DESCRIPTION_LENGTH,
    EXCERPT_MARKER,
    hasPublishedPost,
    isDraftPost,
    postContractViolations,
    postFrontmatter,
} from './blog-posts.mjs';

describe('isDraftPost', () => {
    it.each([
        ['---\ntitle: A\ndraft: true\n---\nbody', true],
        ['---\r\ndraft: true\r\n---\r\n', true],
        ['---\ndraft: true # until reviewed\n---\n', true],
        ['---\ntitle: A\ndraft: false\n---\n', false],
        ['---\ntitle: A\n---\ndraft: true\n', false],
        ['---\nnotdraft: true\n---\n', false],
        ['no frontmatter', false],
    ])('%j -> %s', (source, expected) => {
        expect(isDraftPost(source)).toBe(expected);
    });
});

describe('hasPublishedPost', () => {
    let dir;
    const post = (name, draft) =>
        writeFileSync(
            join(dir, name),
            `---\ntitle: ${name}\n${draft ? 'draft: true\n' : ''}---\n`,
        );

    beforeEach(() => {
        dir = mkdtempSync(join(tmpdir(), 'blog-'));
    });
    afterEach(() => rmSync(dir, { recursive: true, force: true }));

    it('is false for a missing directory', () => {
        expect(hasPublishedPost(join(dir, 'absent'))).toBe(false);
    });

    it('is false when every post is a draft', () => {
        post('a.md', true);
        post('b.mdx', true);
        expect(hasPublishedPost(dir)).toBe(false);
    });

    it('is true once one post is published, nested or not', () => {
        post('a.md', true);
        mkdirSync(join(dir, '2026'));
        writeFileSync(join(dir, '2026', 'b.md'), '---\ntitle: b\n---\n');
        expect(hasPublishedPost(dir)).toBe(true);
    });

    it('ignores non-Markdown files', () => {
        writeFileSync(join(dir, 'notes.txt'), 'hello');
        expect(hasPublishedPost(dir)).toBe(false);
    });
});

describe('postFrontmatter', () => {
    it('reads scalars, inline lists and block lists', () => {
        expect(
            postFrontmatter(
                '---\ntitle: \'A post\'\nauthors: [justin, "ada"]\ntags:\n  - release\n  - design\ndate: 2026-11-09 # day\n---\nbody',
            ),
        ).toEqual({
            title: 'A post',
            authors: ['justin', 'ada'],
            tags: ['release', 'design'],
            date: '2026-11-09',
        });
    });

    it('returns no fields without a frontmatter block', () => {
        expect(postFrontmatter('title: A\n')).toEqual({});
    });
});

describe('postContractViolations', () => {
    const authors = { justin: BLOG_AUTHORS.justin };
    const description = 'd'.repeat(DESCRIPTION_LENGTH.min);
    const post = (fields, body = `Intro.\n\n${EXCERPT_MARKER}\n\nMore.`) =>
        `---\n${fields}\n---\n\n${body}\n`;
    const valid = `title: A\ndescription: ${description}\ndate: 2026-11-09\nauthors: justin\ntags:\n  - release`;

    it('accepts a post that meets the contract', () => {
        expect(postContractViolations(post(valid), authors)).toEqual([]);
    });

    it.each([
        ['title', valid.replace('title: A\n', ''), 'missing title'],
        [
            'short description',
            valid.replace(description, 'short'),
            `description must be ${DESCRIPTION_LENGTH.min}-${DESCRIPTION_LENGTH.max} characters (has 5)`,
        ],
        [
            'long description',
            valid.replace(description, 'd'.repeat(DESCRIPTION_LENGTH.max + 1)),
            `description must be ${DESCRIPTION_LENGTH.min}-${DESCRIPTION_LENGTH.max} characters (has ${DESCRIPTION_LENGTH.max + 1})`,
        ],
        [
            'date',
            valid.replace('date: 2026-11-09\n', ''),
            'missing date (YYYY-MM-DD)',
        ],
        ['authors', valid.replace('authors: justin\n', ''), 'missing authors'],
        [
            'unknown author',
            valid.replace('authors: justin', 'authors: [justin, ada]'),
            'unknown author "ada"',
        ],
        ['tags', valid.replace('\ntags:\n  - release', ''), 'missing tags'],
    ])('reports a missing or invalid %s', (_, fields, message) => {
        expect(postContractViolations(post(fields), authors)).toEqual([
            message,
        ]);
    });

    it('reports a missing excerpt marker', () => {
        expect(
            postContractViolations(post(valid, 'Body only.'), authors),
        ).toEqual([`missing excerpt marker ${EXCERPT_MARKER}`]);
    });

    it('gives every configured author a name, profile link and picture', () => {
        for (const author of Object.values(BLOG_AUTHORS)) {
            expect(author.name).toBeTruthy();
            expect(author.url).toMatch(/^https:\/\/github\.com\/[\w-]+$/);
            const login = author.url.split('/').pop();
            expect(author.picture).toBe(
                `https://avatars.githubusercontent.com/${login}?s=96`,
            );
        }
    });
});
