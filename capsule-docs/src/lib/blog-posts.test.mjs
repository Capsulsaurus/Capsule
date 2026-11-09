import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { hasPublishedPost, isDraftPost } from './blog-posts.mjs';

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
