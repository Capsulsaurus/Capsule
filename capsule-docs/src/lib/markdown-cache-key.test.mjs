import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import markdownCacheKey, {
    CACHE_KEY_OPTION,
    localImports,
    pipelineDigest,
    pipelineFiles,
} from './markdown-cache-key.mjs';

let dir;

/** Write `files` (relative path → source) under the temporary directory. */
function tree(files) {
    for (const [path, source] of Object.entries(files)) {
        const file = join(dir, path);
        mkdirSync(join(file, '..'), { recursive: true });
        writeFileSync(file, source);
    }
}

beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'markdown-cache-key-'));
});

afterEach(() => {
    rmSync(dir, { recursive: true, force: true });
});

describe('localImports', () => {
    it('finds every relative static import, re-export and dynamic import', () => {
        const source = [
            "import a from './a.mjs';",
            "import { b,\n    c } from '../lib/b.mjs';",
            "import './side-effect.js';",
            "export { d } from './d.mjs';",
            "export * from './e.mjs';",
            "const f = await import('./f.mjs');",
        ].join('\n');
        expect(localImports(source)).toEqual([
            './a.mjs',
            '../lib/b.mjs',
            './side-effect.js',
            './d.mjs',
            './e.mjs',
            './f.mjs',
        ]);
    });

    it('leaves packages and node built-ins out: the lockfile covers them', () => {
        const source = [
            "import { readFileSync } from 'node:fs';",
            "import { visit } from 'unist-util-visit';",
            "import x from '@scope/pkg';",
        ].join('\n');
        expect(localImports(source)).toEqual([]);
    });
});

describe('pipelineFiles', () => {
    it('follows local imports transitively, each file once, sorted', () => {
        tree({
            'lib/plugin.mjs':
                "import { x } from './data.mjs';\nimport '../shared/util.mjs';",
            'lib/data.mjs': "import '../shared/util.mjs';",
            'shared/util.mjs': "import { y } from 'some-package';",
        });
        expect(pipelineFiles([join(dir, 'lib/plugin.mjs')])).toEqual(
            [
                join(dir, 'lib/data.mjs'),
                join(dir, 'lib/plugin.mjs'),
                join(dir, 'shared/util.mjs'),
            ].sort(),
        );
    });

    it('survives an import cycle', () => {
        tree({
            'a.mjs': "import './b.mjs';",
            'b.mjs': "import './a.mjs';",
        });
        expect(pipelineFiles([join(dir, 'a.mjs')])).toEqual([
            join(dir, 'a.mjs'),
            join(dir, 'b.mjs'),
        ]);
    });

    it('fails loudly on a missing import rather than hashing less', () => {
        tree({ 'a.mjs': "import './gone.mjs';" });
        expect(() => pipelineFiles([join(dir, 'a.mjs')])).toThrow(/gone\.mjs/);
    });
});

describe('pipelineDigest', () => {
    it('is stable for the same files and changes with any byte of any file', () => {
        tree({
            'a.mjs': 'export const a = 1;',
            'b.mjs': 'export const b = 2;',
        });
        const files = [join(dir, 'a.mjs'), join(dir, 'b.mjs')];
        const before = pipelineDigest(files);
        expect(pipelineDigest(files)).toBe(before);
        expect(pipelineDigest([...files].reverse())).toBe(before);

        tree({ 'b.mjs': 'export const b = 3;' });
        expect(pipelineDigest(files)).not.toBe(before);
    });

    it('tells a moved byte between files from no change', () => {
        tree({ 'a.mjs': 'ab', 'b.mjs': 'c' });
        const files = [join(dir, 'a.mjs'), join(dir, 'b.mjs')];
        const before = pipelineDigest(files);
        tree({ 'a.mjs': 'a', 'b.mjs': 'bc' });
        expect(pipelineDigest(files)).not.toBe(before);
    });
});

describe('markdownCacheKey integration', () => {
    /** Run the integration's config hook against a fake Astro config. */
    function setup(entries, extra) {
        const config = { markdown: { processor: { options: {} } } };
        const addWatchFile = vi.fn();
        const logger = { info: vi.fn(), debug: vi.fn() };
        markdownCacheKey({ entries, extra }).hooks['astro:config:setup']({
            config,
            addWatchFile,
            logger,
        });
        return { config, addWatchFile, logger };
    }

    it('stamps the processor with the digest, so a plugin edit changes Astro’s config digest', () => {
        tree({
            'plugin.mjs': "import './data.mjs';",
            'data.mjs': 'export const v = 1;',
            'bun.lock': '{}',
        });
        const entries = [join(dir, 'plugin.mjs')];
        const extra = [join(dir, 'bun.lock')];
        const before = setup(entries, extra).config.markdown.processor.options[
            CACHE_KEY_OPTION
        ];
        expect(before).toMatch(/^[0-9a-f]{64}$/);

        tree({ 'data.mjs': 'export const v = 2;' });
        const after = setup(entries, extra).config.markdown.processor.options[
            CACHE_KEY_OPTION
        ];
        expect(after).not.toBe(before);

        tree({ 'bun.lock': '{"changed":true}' });
        expect(
            setup(entries, extra).config.markdown.processor.options[
                CACHE_KEY_OPTION
            ],
        ).not.toBe(after);
    });

    it('watches every file it hashes, so the dev server restarts on an edit', () => {
        tree({
            'plugin.mjs': "import './data.mjs';",
            'data.mjs': '',
            'bun.lock': '',
        });
        const { addWatchFile } = setup(
            [join(dir, 'plugin.mjs')],
            [join(dir, 'bun.lock')],
        );
        expect(addWatchFile.mock.calls.map(([file]) => file).sort()).toEqual(
            [
                join(dir, 'bun.lock'),
                join(dir, 'data.mjs'),
                join(dir, 'plugin.mjs'),
            ].sort(),
        );
    });

    it('accepts file URLs, as astro.config.mjs writes them', () => {
        tree({ 'plugin.mjs': '' });
        const { addWatchFile } = setup(
            [new URL(`file://${join(dir, 'plugin.mjs')}`)],
            [],
        );
        expect(addWatchFile).toHaveBeenCalledWith(join(dir, 'plugin.mjs'));
    });
});
