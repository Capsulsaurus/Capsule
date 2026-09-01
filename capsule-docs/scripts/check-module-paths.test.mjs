import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { checkModulePaths, resolveModulePath } from './check-module-paths.mjs';

let root;

function repo(files) {
    root = mkdtempSync(join(tmpdir(), 'capsule-module-paths-'));
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

const DOC = 'capsule-docs/src/content/docs/design/x.md';

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

describe('resolveModulePath', () => {
    it('resolves a module directory and a module file', () => {
        const r = repo({
            'capsule-core/src/crypto/keys.rs': '',
            'capsule-core/src/library/mod.rs': '',
        });
        expect(resolveModulePath(r, 'capsule-core::crypto::keys')).toBeNull();
        expect(resolveModulePath(r, 'capsule-core::library')).toBeNull();
    });

    it('accepts an underscored crate spelling', () => {
        const r = repo({ 'capsule-core/src/backup.rs': '' });
        expect(resolveModulePath(r, 'capsule_core::backup')).toBeNull();
    });

    it('resolves a leaf that is a pub use re-export, not a file', () => {
        // `capsule-core::library::available_bytes` is a re-export in
        // `library/mod.rs`. A resolver that demands a file calls it missing.
        const r = repo({
            'capsule-core/src/library/mod.rs':
                'pub use space::{available_bytes, reclaim};\n',
            'capsule-core/src/library/space.rs':
                'pub fn available_bytes() {}\n',
        });
        expect(
            resolveModulePath(r, 'capsule-core::library::available_bytes'),
        ).toBeNull();
    });

    it('resolves a leaf that is a declared item', () => {
        const r = repo({
            'capsule-core/src/lifecycle/mod.rs': 'pub struct Workspace;\n',
        });
        expect(
            resolveModulePath(r, 'capsule-core::lifecycle::Workspace'),
        ).toBeNull();
    });

    it('reports a missing module and lists its siblings', () => {
        const r = repo({
            'capsule-server/src/album/mod.rs': '',
            'capsule-server/src/upload/mod.rs': '',
        });
        const reason = resolveModulePath(r, 'capsule-server::organization');
        expect(reason).toContain('no `organization`');
        expect(reason).toContain('has: album, upload');
    });

    it('reports an unknown crate', () => {
        const r = repo({ 'capsule-core/src/lib.rs': '' });
        expect(resolveModulePath(r, 'capsule-api::upload')).toContain(
            'no crate `capsule-api`',
        );
    });
});

describe('checkModulePaths', () => {
    it('expands a brace group into one path per member', () => {
        const { findings, checked } = checkModulePaths(
            repo({
                'capsule-core/src/library/mod.rs': '',
                [DOC]: '`capsule-core::{library,db}`\n',
            }),
        );
        expect(checked).toBe(2);
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('capsule-core::db');
    });

    it('accepts a planned module, and everything beneath it', () => {
        const { findings } = checkModulePaths(
            repo({
                'capsule-core/src/lib.rs': '',
                'capsule-docs/planned-modules.txt':
                    'capsule-core::media\tconsumes Rawshift once it stabilizes\n',
                [DOC]: '`capsule-core::media` and `capsule-core::media::video::derivative`\n',
            }),
        );
        expect(findings).toEqual([]);
    });

    it('reports a planned module that has since been built', () => {
        const { findings } = checkModulePaths(
            repo({
                'capsule-core/src/media/mod.rs': '',
                'capsule-docs/planned-modules.txt':
                    'capsule-core::media\tstale\n',
                [DOC]: '`capsule-core::media`\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('exists now');
    });

    it('reports a planned entry no doc names any more', () => {
        const { findings } = checkModulePaths(
            repo({
                'capsule-core/src/lib.rs': '',
                'capsule-docs/planned-modules.txt':
                    'capsule-core::gone\tstale\n',
                [DOC]: 'no citations\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('no doc names');
    });

    it('scans AGENTS.md but not SLICES.md, which is a historical ledger', () => {
        const { findings } = checkModulePaths(
            repo({
                'capsule-core/src/lib.rs': '',
                'AGENTS.md': '`capsule-core::gone`\n',
                'SLICES.md': '`capsule-api::retired`\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('AGENTS.md');
    });
});
