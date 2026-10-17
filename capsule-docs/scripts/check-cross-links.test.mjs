import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { checkCrossLinks } from './check-cross-links.mjs';

let root;

/** Materialize a throwaway repository from a path -> contents map. */
function repo(files) {
    root = mkdtempSync(join(tmpdir(), 'capsule-cross-links-'));
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

/** An Astro config carrying just the `site` value the check reads back. */
const ASTRO_CONFIG =
    "export default defineConfig({ site: 'https://capsule.example.net' });\n";

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

describe('checkCrossLinks', () => {
    it('reports a repo-relative link to a missing file', () => {
        const { findings } = checkCrossLinks(
            repo({
                'SLICES.md':
                    '[Drops](capsule-docs/src/content/docs/design/drops.md)\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('SLICES.md:1');
        expect(findings[0]).toContain('no such file');
    });

    it('accepts a repo-relative link that resolves', () => {
        const { findings, checked } = checkCrossLinks(
            repo({
                'SLICES.md':
                    '[Web Upload](capsule-docs/src/content/docs/design/web-upload.md)\n',
                'capsule-docs/src/content/docs/design/web-upload.md':
                    '# Web Upload\n',
            }),
        );
        expect(findings).toEqual([]);
        expect(checked).toBe(1);
    });

    it('accepts a relative link to a directory, which GitHub renders as a tree', () => {
        const { findings } = checkCrossLinks(
            repo({
                'SLICES.md':
                    '[design docs](capsule-docs/src/content/docs/design/)\n',
                'capsule-docs/src/content/docs/design/keys.md': '# Keys\n',
            }),
        );
        expect(findings).toEqual([]);
    });

    it('reports a leading-slash link outside the site, which resolves to nothing on GitHub', () => {
        const { findings } = checkCrossLinks(
            repo({
                'CONTRIBUTING.md':
                    '[Development](/capsule-docs/src/content/docs/development/)\n',
                'capsule-docs/src/content/docs/development/architecture.md':
                    '# A\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('a leading slash is not repo-relative');
    });

    it('leaves root-relative links inside the site to starlight-links-validator', () => {
        const { findings } = checkCrossLinks(
            repo({
                'capsule-docs/src/content/docs/design/a.md':
                    '[b](/design/b/)\n',
            }),
        );
        expect(findings).toEqual([]);
    });

    it('resolves a published-site URL to the page that serves it', () => {
        const files = {
            'capsule-docs/astro.config.mjs': ASTRO_CONFIG,
            'capsule-docs/src/content/docs/design/thumbnails.md':
                '# T\n\n## LQIP\n',
            'README.md':
                '[T](https://capsule.example.net/design/thumbnails/#lqip)\n',
        };
        expect(checkCrossLinks(repo(files)).findings).toEqual([]);

        files['README.md'] = '[T](https://capsule.example.net/design/gone/)\n';
        expect(checkCrossLinks(repo(files)).findings[0]).toContain(
            'no page serves this route',
        );
    });

    it('reports an anchor that no heading in the target slugs to', () => {
        const { findings } = checkCrossLinks(
            repo({
                'AGENTS.md':
                    '[LQIP](capsule-docs/src/content/docs/design/thumbnails.md#lqip)\n',
                'capsule-docs/src/content/docs/design/thumbnails.md':
                    '# T\n\n## Previews\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('has no #lqip');
    });

    it('skips legacy-review, whose links record trees that were deleted on purpose', () => {
        const { findings } = checkCrossLinks(
            repo({
                'legacy-review/sdk-progenitor/README.md':
                    '[api](../capsule-api/auth/README.md)\n',
            }),
        );
        expect(findings).toEqual([]);
    });

    it('survives an anchor that is not valid percent-encoding', () => {
        // `decodeURIComponent('50%-of-quota')` throws URIError; an uncaught one
        // kills the run with a stack trace instead of producing a report.
        const { findings } = checkCrossLinks(
            repo({
                'AGENTS.md':
                    '[q](capsule-docs/src/content/docs/design/quota.md#50%-of-quota)\n',
                'capsule-docs/src/content/docs/design/quota.md':
                    '# Q\n\n## 50%-of-quota\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('has no #50%-of-quota');
    });

    it('percent-decodes the path half before resolving it', () => {
        const { findings } = checkCrossLinks(
            repo({
                'README.md': '[x](capsule-docs/my%20file.md)\n',
                'capsule-docs/my file.md': '# X\n',
            }),
        );
        expect(findings).toEqual([]);
    });

    it('ignores external links and does not follow the docs symlink target twice', () => {
        const { findings, checked } = checkCrossLinks(
            repo({
                'README.md':
                    '[x](https://example.com/a) and [y](mailto:a@b.c)\n',
            }),
        );
        expect(findings).toEqual([]);
        expect(checked).toBe(0);
    });
});
