import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
    checkAgentRules,
    parseRulePaths,
    trackedFiles,
} from './check-agent-rules.mjs';

let root;

/** Materialize a throwaway repository from a path -> contents map. */
function repo(files) {
    root = mkdtempSync(join(tmpdir(), 'capsule-agent-rules-'));
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

/** A rule file scoped to `globs`. */
function rule(...globs) {
    return `---\npaths:\n${globs.map((g) => `  - "${g}"`).join('\n')}\n---\n\n# Rule\n`;
}

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

describe('parseRulePaths', () => {
    it('reads a quoted and an unquoted block list', () => {
        expect(
            parseRulePaths(
                '---\npaths:\n  - "a/**"\n  - b/*.rs\n  - \'c/{x,y}.md\'\n---\nbody\n',
            ),
        ).toEqual(['a/**', 'b/*.rs', 'c/{x,y}.md']);
    });

    it('returns null without front matter or without a paths key', () => {
        expect(parseRulePaths('# Rule\n')).toBeNull();
        expect(parseRulePaths('---\ndescription: x\n---\n')).toBeNull();
    });

    it('stops at the next key', () => {
        expect(
            parseRulePaths('---\npaths:\n  - "a/**"\nother: 1\n---\n'),
        ).toEqual(['a/**']);
    });

    it('reads past blank and comment lines inside the list', () => {
        // Ending the list early would leave the later globs unchecked.
        expect(
            parseRulePaths(
                '---\npaths:\n  - "a/**"\n\n  # apps\n  - "b/**" # web\n---\n',
            ),
        ).toEqual(['a/**', 'b/**']);
    });

    it('reads an unindented list, an inline list, CRLF and a BOM', () => {
        expect(parseRulePaths('---\npaths:\n- "a/**"\n---\n')).toEqual([
            'a/**',
        ]);
        expect(parseRulePaths('---\npaths: ["a/**", b/*.rs]\n---\n')).toEqual([
            'a/**',
            'b/*.rs',
        ]);
        expect(
            parseRulePaths('﻿---\r\npaths:\r\n  - "a/**"\r\n---\r\n'),
        ).toEqual(['a/**']);
    });

    it('keeps a brace glob whole inside an inline list', () => {
        expect(
            parseRulePaths('---\npaths: ["a/{x,y}/**", \'b,c/*\']\n---\n'),
        ).toEqual(['a/{x,y}/**', 'b,c/*']);
    });
});

describe('trackedFiles', () => {
    it('lists the git index, not untracked files, inside a work tree', () => {
        const r = repo({
            'capsule-core/Cargo.toml': '',
            'scratch/notes.md': '',
        });
        execFileSync('git', ['init', '-q'], { cwd: r });
        execFileSync('git', ['add', 'capsule-core/Cargo.toml'], { cwd: r });
        expect(trackedFiles(r)).toEqual(['capsule-core/Cargo.toml']);
    });

    it('walks the filesystem outside a work tree', () => {
        const r = repo({ 'a/b.md': '' });
        expect(trackedFiles(r)).toEqual(['a/b.md']);
    });
});

describe('checkAgentRules', () => {
    it('passes a covered crate', () => {
        const { findings, checked } = checkAgentRules(
            repo({
                '.claude/rules/core.md': rule('capsule-core/**'),
                'capsule-core/Cargo.toml': '',
                'capsule-core/src/lib.rs': '',
            }),
        );
        expect(findings).toEqual([]);
        expect(checked).toBe(1);
    });

    it('reports a rule without paths front matter', () => {
        const { findings } = checkAgentRules(
            repo({
                '.claude/rules/loose.md': '# Always on\n',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('.claude/rules/loose.md');
        expect(findings[0]).toContain('no `paths:`');
    });

    it('reports a glob that matches no file', () => {
        // A renamed directory leaves its rule silently never loading.
        const { findings } = checkAgentRules(
            repo({
                '.claude/rules/core.md': rule(
                    'capsule-core/**',
                    'capsule-gone/**',
                ),
                'capsule-core/Cargo.toml': '',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('capsule-gone/**');
        expect(findings[0]).toContain('matches no tracked file');
    });

    it('reports a component directory no rule scopes to', () => {
        // `**/*.rs` matches the crate, but a repo-wide glob is not coverage:
        // the crate still has no rule naming its own docs.
        const { findings } = checkAgentRules(
            repo({
                '.claude/rules/rust.md': rule('**/*.rs'),
                'capsule-new/Cargo.toml': '',
                'capsule-new/src/lib.rs': '',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('capsule-new/');
        expect(findings[0]).toContain('no rule');
    });

    it("does not count another area's partial glob as coverage", () => {
        // Deleting the CLI's own rule must fail even though the i18n rule
        // still scopes `capsule-cli/src/**/*.rs`.
        const { findings } = checkAgentRules(
            repo({
                '.claude/rules/i18n.md': rule('capsule-cli/src/**/*.rs'),
                'capsule-cli/Cargo.toml': '',
                'capsule-cli/src/main.rs': '',
            }),
        );
        expect(findings).toEqual([
            'capsule-cli/  no rule in .claude/rules/ is scoped to `capsule-cli/**`',
        ]);
    });

    it('checks rule files in subdirectories of .claude/rules/', () => {
        const { findings } = checkAgentRules(
            repo({
                '.claude/rules/core.md': rule('capsule-core/**'),
                '.claude/rules/nested/extra.md': '# No front matter\n',
                'capsule-core/Cargo.toml': '',
            }),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain(
            '.claude/rules/nested/extra.md  no `paths:`',
        );
    });

    it('checks globs against the file list it is given', () => {
        // In a git work tree that list is `git ls-files`: a glob matching only
        // an untracked or ignored file passes nowhere but the author's disk.
        const r = repo({
            '.claude/rules/core.md': rule('capsule-core/**'),
            'capsule-core/Cargo.toml': '',
        });
        const { findings } = checkAgentRules(r, ['.claude/rules/core.md']);
        expect(findings).toEqual([
            '.claude/rules/core.md  capsule-core/**  matches no tracked file; the rule never loads for it',
        ]);
    });

    it('recognises every manifest kind and exempts legacy-review', () => {
        const { findings } = checkAgentRules(
            repo({
                '.claude/rules/apps.md': rule('web/**'),
                'web/package.json': '{}',
                'android/build.gradle.kts': '',
                'swift/Project.swift': '',
                'pkg/Package.swift': '',
                'legacy-review/Cargo.toml': '',
            }),
        );
        expect(findings.map((f) => f.split('  ')[0]).sort()).toEqual([
            'android/',
            'pkg/',
            'swift/',
        ]);
    });

    it('is silent in a repository with no rules directory', () => {
        const { findings, checked } = checkAgentRules(
            repo({ 'README.md': '# x\n' }),
        );
        expect(findings).toEqual([]);
        expect(checked).toBe(0);
    });
});
