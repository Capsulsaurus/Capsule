import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { checkSourceLinks } from './check-source-links.mjs';

let root;

function repo(files) {
    root = mkdtempSync(join(tmpdir(), 'capsule-source-links-'));
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

const PAGE = 'capsule-docs/src/content/docs/design/transfer.md';

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

describe('checkSourceLinks', () => {
    it('accepts a relative Markdown link and its anchor', () => {
        const { findings, checked } = checkSourceLinks(
            repo({
                [PAGE]: '# Transfer\n\n## Bounds\n',
                'capsule-server/src/limits.rs':
                    '//! [Bounds](../../capsule-docs/src/content/docs/design/transfer.md#bounds)\n',
            }),
        );
        expect(findings).toEqual([]);
        expect(checked).toBe(1);
    });

    it('reports a relative link at the wrong depth', () => {
        const { findings } = checkSourceLinks(
            repo({
                [PAGE]: '# Transfer\n',
                'capsule-server/src/album/mod.rs':
                    '//! [t](../../capsule-docs/src/content/docs/design/transfer.md)\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('capsule-server/src/album/mod.rs:1');
        expect(findings[0]).toContain('no such page');
    });

    it('resolves a bare site path against the content tree', () => {
        const { findings } = checkSourceLinks(
            repo({
                [PAGE]: '# Transfer\n\n## Sessions\n',
                'capsule-sdk/src/upload.rs':
                    '// see design/transfer.md#sessions and design/gone.md\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('design/gone.md');
    });

    it('reports a missing anchor', () => {
        const { findings } = checkSourceLinks(
            repo({
                [PAGE]: '# Transfer\n',
                'capsule-sdk/src/fetch.rs': '// design/transfer.md#ranges\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('has no #ranges');
    });

    it('does not double-count the tail of a Markdown link target', () => {
        const { checked } = checkSourceLinks(
            repo({
                [PAGE]: '# Transfer\n',
                'capsule-core/src/lib.rs':
                    '//! [t](../../capsule-docs/src/content/docs/design/transfer.md)\n',
            }),
        );
        expect(checked).toBe(1);
    });

    it('skips legacy-review and non-source files', () => {
        const { checked } = checkSourceLinks(
            repo({
                'legacy-review/old.rs': '// design/missing.md\n',
                'notes.txt': 'design/missing.md\n',
            }),
        );
        expect(checked).toBe(0);
    });
});
