import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { contentFileForRoute, servingFileForRoute } from './routes.mjs';

let dir;

beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), 'capsule-routes-'));
    for (const rel of [
        'index.mdx',
        'design/index.md',
        'design/keys.md',
        'guides/start.md',
        'ja/index.mdx',
        'ja/guides/start.md',
    ]) {
        mkdirSync(dirname(join(dir, rel)), { recursive: true });
        writeFileSync(join(dir, rel), '# x\n');
    }
});

afterAll(() => rmSync(dir, { recursive: true, force: true }));

describe('contentFileForRoute', () => {
    it('resolves a page, a section index and the front page', () => {
        expect(contentFileForRoute(dir, '/design/keys/')).toBe(
            'design/keys.md',
        );
        expect(contentFileForRoute(dir, '/design/')).toBe('design/index.md');
        expect(contentFileForRoute(dir, '/')).toBe('index.mdx');
        expect(contentFileForRoute(dir, '/gone/')).toBeNull();
    });

    it("resolves a locale's route only to its own translation", () => {
        expect(contentFileForRoute(dir, '/ja/guides/start/')).toBe(
            'ja/guides/start.md',
        );
        expect(contentFileForRoute(dir, '/ja/design/keys/')).toBeNull();
    });
});

describe('servingFileForRoute', () => {
    it('serves a translation where one exists', () => {
        expect(servingFileForRoute(dir, '/ja/guides/start/')).toBe(
            'ja/guides/start.md',
        );
        expect(servingFileForRoute(dir, '/ja/')).toBe('ja/index.mdx');
    });

    it('falls back to the English page under a locale with no translation', () => {
        expect(servingFileForRoute(dir, '/ja/design/keys/')).toBe(
            'design/keys.md',
        );
        expect(servingFileForRoute(dir, '/ko/')).toBe('index.mdx');
        expect(servingFileForRoute(dir, 'ko/design')).toBe('design/index.md');
    });

    it('finds nothing where neither serves the route', () => {
        expect(servingFileForRoute(dir, '/ja/gone/')).toBeNull();
        expect(servingFileForRoute(dir, '/xx/design/keys/')).toBeNull();
    });
});
