import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
    checkRoadmap,
    declaredPackages,
    definedStates,
    miseTasks,
    reportRoadmap,
    sliceIds,
    tables,
} from './check-roadmap.mjs';

let root;

function repo(files) {
    root = mkdtempSync(join(tmpdir(), 'capsule-roadmap-'));
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

const STATES = `## States

- \`frozen\` — ships, contract settled.
- \`stabilizing\` — live and gated.
- \`rebuilding\` — quarantined and being re-landed.
- \`blocked\` — a named dependency gates it.
- \`deferred\` — deliberately unscheduled.
- \`review-only\` — reference material.
- \`excluded\` — outside the shipped build.
`;

const HEADER =
    '| Package | Kind | Owns | State | Gate | Owner docs | Open slices | Next milestone | Notes |\n' +
    '| --- | --- | --- | --- | --- | --- | --- | --- | --- |';

/** A `ROADMAP.md` whose package table is exactly `rows`. */
function roadmap(rows) {
    return `# Roadmap\n\n${STATES}\n## Packages\n\n${HEADER}\n${rows.join('\n')}\n`;
}

/** One well-formed row for a cargo package with no open slices. */
function row(pkg, overrides = {}) {
    const cell = {
        kind: 'cargo',
        owns: 'things',
        state: 'stabilizing',
        gate: '`mise run check-rust`',
        docs: '[d](d.md)',
        open: '—',
        next: 'later',
        notes: '—',
        ...overrides,
    };
    return `| \`${pkg}\` | ${cell.kind} | ${cell.owns} | ${cell.state} | ${cell.gate} | ${cell.docs} | ${cell.open} | ${cell.next} | ${cell.notes} |`;
}

/** The minimum tree the check reads besides `ROADMAP.md`. */
const BASE = {
    'Cargo.toml': '[workspace]\nmembers = [\n    "alpha",\n]\n',
    'mise.toml': '[tasks.check-rust]\nrun = "true"\n',
    'SLICES.md': '### S-A1 — a slice\n\n### S-B2 — another slice\n',
};

describe('tables', () => {
    it('reads a table into a header and rows carrying their line numbers', () => {
        const [table] = tables(
            'intro\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n',
        );
        expect(table.header).toEqual(['A', 'B']);
        expect(table.rows).toEqual([{ cells: ['1', '2'], line: 5 }]);
    });

    it('separates two tables rather than running them together', () => {
        const found = tables(
            '| A |\n| --- |\n| 1 |\n\n| B |\n| --- |\n| 2 |\n',
        );
        expect(found.map((t) => t.header)).toEqual([['A'], ['B']]);
    });

    it('ignores a pipe line that no separator follows', () => {
        expect(tables('| not a table |\nprose\n')).toEqual([]);
    });
});

describe('definedStates', () => {
    it('reads the vocabulary under the States heading, in order', () => {
        expect(definedStates(`# R\n\n${STATES}\n## Packages\n`)).toEqual([
            'frozen',
            'stabilizing',
            'rebuilding',
            'blocked',
            'deferred',
            'review-only',
            'excluded',
        ]);
    });

    it('stops at the next section, so a later bullet is not a state', () => {
        const source = `${STATES}\n## Packages\n\n- \`sneaky\` — not a state.\n`;
        expect(definedStates(source)).not.toContain('sneaky');
    });

    it('returns nothing when the document defines no vocabulary', () => {
        expect(definedStates('# Roadmap\n\n## Packages\n')).toEqual([]);
    });
});

describe('declaredPackages', () => {
    it('reads workspace members and skips default-members', () => {
        const r = repo({
            'Cargo.toml':
                '[workspace]\nmembers = [\n    "alpha",\n    # "commented",\n    "beta/gamma",\n]\ndefault-members = [\n    "alpha",\n]\n',
        });
        expect([...declaredPackages(r)]).toEqual([
            ['alpha', 'cargo'],
            ['beta/gamma', 'cargo'],
        ]);
    });

    it('resolves a gradle include to the directory project() names', () => {
        const r = repo({
            'settings.gradle.kts':
                'include(":android")\nproject(":android").projectDir = file("capsule-android")\n',
        });
        expect(declaredPackages(r).get('capsule-android')).toBe('gradle');
    });

    it('does not see a commented-out gradle include', () => {
        // `settings.gradle.kts` keeps `:cli`/`:desktop` commented out; a row for
        // either would then be an orphan, which is the finding to avoid.
        const r = repo({
            'settings.gradle.kts':
                '// include(":desktop")\n// project(":desktop").projectDir = file("capsule-desktop")\n',
        });
        expect(declaredPackages(r).size).toBe(0);
    });

    it('reads tuist module targets across line breaks, plus the app target', () => {
        const r = repo({
            'capsule-swift/Project.swift':
                'private func module(\n    _ name: String\n) -> [Target] { [] }\n' +
                'let moduleTargets: [Target] = module("CapsuleFoundation")\n' +
                '    + module(\n        "FeatureAlbums",\n        dependencies: []\n    )\n' +
                'private let appTarget: Target = .target(\n    name: "Capsule",\n    product: .app\n)\n',
        });
        expect([...declaredPackages(r).keys()]).toEqual([
            'CapsuleFoundation',
            'FeatureAlbums',
            'Capsule',
        ]);
    });

    it('reads a conditional tuist target, which therefore needs a row', () => {
        const r = repo({
            'capsule-swift/Project.swift':
                'let t = ffiEnabled\n    ? module(\n        "CapsuleCatalogFFI"\n    )\n    : []\n',
        });
        expect(declaredPackages(r).get('CapsuleCatalogFFI')).toBe('tuist');
    });

    it('classifies a package root by the manifest it carries', () => {
        const r = repo({
            'capsule-core-swift/Package.swift': '// swift-tools-version:6.0\n',
            'capsule-web/package.json': '{}\n',
            'capsule-vision/pyproject.toml': '[project]\n',
            'locales/en.json': '{}\n',
            '.gitmodules': '[submodule "rawshift"]\n\tpath = rawshift\n',
            'legacy-review/server-salvo/REVIEW.md': '',
        });
        const declared = declaredPackages(r);
        expect(declared.get('capsule-core-swift')).toBe('swiftpm');
        expect(declared.get('capsule-web')).toBe('bun');
        expect(declared.get('capsule-vision')).toBe('python');
        expect(declared.get('locales')).toBe('catalog');
        expect(declared.get('rawshift')).toBe('submodule');
        expect(declared.get('legacy-review/server-salvo')).toBe(
            'review-bucket',
        );
    });
});

describe('miseTasks and sliceIds', () => {
    it('reads tasks from both manifests and from the file-task directory', () => {
        const r = repo({
            'mise.toml': '[tasks.check-rust]\nrun = "true"\n',
            'capsule-swift/mise.toml': '[tasks.generate]\nrun = "true"\n',
            'mise-tasks/check-swift': '#!/usr/bin/env bash\n',
        });
        expect([...miseTasks(r)].sort()).toEqual([
            'check-rust',
            'check-swift',
            'generate',
        ]);
    });

    it('reads slice ids from detail headings only', () => {
        const r = repo({
            'SLICES.md':
                '| S-Z9 | an index row | | | | | |\n\n### S-A1 — a slice\n\n#### S-A2 — not a detail block\n',
        });
        expect([...sliceIds(r)]).toEqual(['S-A1']);
    });
});

describe('checkRoadmap', () => {
    it('passes a roadmap whose rows all resolve', () => {
        const r = repo({ ...BASE, 'ROADMAP.md': roadmap([row('alpha')]) });
        const result = checkRoadmap(r);
        expect(result.findings).toEqual([]);
        expect(result.checked).toBe(1);
        expect(reportRoadmap(result)).toContain('1 package(s) checked');
    });

    it('fails when a declared package has no row', () => {
        const r = repo({
            ...BASE,
            'Cargo.toml':
                '[workspace]\nmembers = [\n    "alpha",\n    "beta",\n]\n',
            'ROADMAP.md': roadmap([row('alpha')]),
        });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md  cargo package `beta` has no row',
        ]);
    });

    it('fails on a row naming nothing in the tree', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha'), row('ghost')]),
        });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md:18  ghost is not declared by any manifest in the tree',
        ]);
    });

    it('fails when a row claims the wrong kind', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha', { kind: 'bun' })]),
        });
        expect(checkRoadmap(r).findings[0]).toContain(
            'alpha is declared as `cargo`, the row says `bun`',
        );
    });

    it('fails on a state outside the defined vocabulary', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha', { state: 'nearly-done' })]),
        });
        expect(checkRoadmap(r).findings[0]).toContain(
            'state `nearly-done` is not one of',
        );
    });

    it('fails on a slice id with no detail block', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha', { open: '`S-A1`, `S-Z9`' })]),
        });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md:17  `S-Z9` has no detail block in SLICES.md',
        ]);
    });

    it('fails on a slice cell that is not an id at all', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha', { open: 'lane B' })]),
        });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md:17  `lane B` is not a slice id',
        ]);
    });

    it('fails on a gate naming a task the repository does not have', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([
                row('alpha', { gate: '`mise run check-moon`' }),
            ]),
        });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md:17  `mise run check-moon` is not a task in this repository',
        ]);
    });

    it('fails on a gate that is not a mise invocation', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha', { gate: '`cargo test`' })]),
        });
        expect(checkRoadmap(r).findings[0]).toContain(
            'gate `cargo test` is not `mise run <task>`',
        );
    });

    it('lets a review-only or excluded row leave the gate empty, and no other', () => {
        const gateless = { gate: '—' };
        const ok = repo({
            ...BASE,
            'ROADMAP.md': roadmap([
                row('alpha', { ...gateless, state: 'review-only' }),
            ]),
        });
        expect(checkRoadmap(ok).findings).toEqual([]);
        rmSync(root, { recursive: true, force: true });

        const bad = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha', gateless)]),
        });
        expect(checkRoadmap(bad).findings[0]).toContain(
            'may leave `Gate` empty',
        );
    });

    it('fails on a row with the wrong number of columns', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap(['| `alpha` | cargo | things |']),
        });
        // A malformed row is skipped whole, so the package it meant to cover is
        // also reported as unrowed. Both findings point at the same repair.
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md:17  3 column(s), expected 9',
            'ROADMAP.md  cargo package `alpha` has no row',
        ]);
    });

    it('fails on a duplicated package row', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': roadmap([row('alpha'), row('alpha')]),
        });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md:18  alpha has more than one row',
        ]);
    });

    it('fails when the header is not the nine agreed columns', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': `# R\n\n${STATES}\n| Package | Kind | Owns | State | Gate | Owner docs | Open slices | Next milestone | Remarks |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n${row('alpha')}\n`,
        });
        expect(checkRoadmap(r).findings[0]).toContain(
            'expected `Package | Kind',
        );
    });

    it('reports an unused state without failing, and counts a second table', () => {
        // Forcing every defined state into use would push whoever edits the file
        // into a dishonest assignment, so this is a report, not a finding.
        const r = repo({ ...BASE, 'ROADMAP.md': roadmap([row('alpha')]) });
        const result = checkRoadmap(r);
        expect(result.findings).toEqual([]);
        expect(result.unused).toContain('deferred');
        expect(reportRoadmap(result)).toContain('States defined and unused');
    });

    it('counts a state used only by the deferred register', () => {
        const register =
            '| Item | Owner docs | State | Notes |\n| --- | --- | --- | --- |\n| a thing | [d](d.md) | deferred | later |';
        const r = repo({
            ...BASE,
            'ROADMAP.md': `${roadmap([row('alpha')])}\n${register}\n`,
        });
        expect(checkRoadmap(r).unused).not.toContain('deferred');
    });

    it('fails when the file is missing entirely', () => {
        const r = repo(BASE);
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md  the package view is missing',
        ]);
    });

    it('fails when the file carries no package table', () => {
        const r = repo({ ...BASE, 'ROADMAP.md': `# R\n\n${STATES}\n` });
        expect(checkRoadmap(r).findings).toEqual([
            'ROADMAP.md  no package table (its first column is `Package`)',
        ]);
    });

    it('fails when the file defines no state vocabulary', () => {
        const r = repo({
            ...BASE,
            'ROADMAP.md': `# R\n\n## Packages\n\n${HEADER}\n${row('alpha')}\n`,
        });
        expect(checkRoadmap(r).findings[0]).toContain('no state vocabulary');
    });
});
