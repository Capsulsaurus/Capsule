// The docs site's fonts are vendored files declared in global.css: every
// @font-face must name a file that exists, and nothing may pull a font from a
// package or another origin instead.
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const stylesDir = resolve(dirname(fileURLToPath(import.meta.url)), '../styles');
const css = readFileSync(resolve(stylesDir, 'global.css'), 'utf8');
const faces = css.match(/@font-face\s*\{[^}]*\}/g) ?? [];

describe('global.css fonts', () => {
    it('declares Geist upright, Geist italic and Geist Mono', () => {
        const families = faces.map(
            (face) =>
                `${face.match(/font-family:\s*"([^"]+)"/)?.[1]}/${face.match(/font-style:\s*(\w+)/)?.[1]}`,
        );
        expect(families).toEqual([
            'Geist/normal',
            'Geist/italic',
            'Geist Mono/normal',
        ]);
    });

    it('points every @font-face at a vendored woff2 that exists', () => {
        for (const face of faces) {
            const urls = [...face.matchAll(/url\("([^"]+)"\)/g)].map(
                (m) => m[1],
            );
            expect(urls).toHaveLength(1);
            expect(urls[0]).toMatch(/^\.\.\/assets\/fonts\/[\w-]+\.woff2$/);
            expect(existsSync(resolve(stylesDir, urls[0]))).toBe(true);
        }
    });

    it('imports no font package and no remote stylesheet', () => {
        expect(css).not.toMatch(/@fontsource/);
        expect(css).not.toMatch(/@import\s+(url\()?["']?https?:/);
    });
});
