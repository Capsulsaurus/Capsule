/**
 * `ROADMAP.md` against the manifests that declare the packages.
 *
 * `SLICES.md` tracks slices; `ROADMAP.md` tracks **packages**, one row each,
 * across every toolchain in the repository. A package-level view is only worth
 * reading if it cannot quietly fall behind the tree, so this check resolves
 * every row against the manifest that declares the package rather than against
 * a second committed list. A committed list would be tautological: both files
 * are hand-edited, so a package added to neither passes.
 *
 * The oracles are deliberately regex over manifest *text*, not parsers:
 *
 *   - `Cargo.toml`'s `[workspace] members`
 *   - `settings.gradle.kts`'s unconditional `include(":x")` plus the
 *     `project(":x").projectDir = file("y")` that names its directory
 *   - `capsule-swift/Project.swift`'s `module("Name"` targets and its app target
 *   - a root-level directory holding `Package.swift` (SwiftPM), `package.json`
 *     (bun) or `pyproject.toml` (uv)
 *   - `locales/`, `.gitmodules`, and the `legacy-review/<bucket>/` directories
 *
 * That keeps this file inside `docs-truth`'s no-dependency, no-toolchain rule
 * (`docs-truth.mjs`), which is what lets a docs-only pull request run it without
 * paying for a cargo, gradle, tuist or uv resolve.
 *
 * **A conditional target is invisible here, on purpose.** `Project.swift` wraps
 * `CapsuleCatalogFFI` in a `ffiEnabled ? … : []` ternary and
 * `settings.gradle.kts` keeps `:cli`/`:desktop` commented out. A regex cannot
 * evaluate either condition, so the rule is: only unconditional declarations are
 * oracles, and a conditional target earns a row whose `State` says `excluded`.
 * `CapsuleCatalogFFI` still matches `module("` and so is required to have a row;
 * the commented-out Gradle modules match nothing and so must not have one.
 */

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

/** The package view. */
const ROADMAP = 'ROADMAP.md';

/** The slice tracker whose ids the `Open slices` column cites. */
const SLICES = 'SLICES.md';

/** Columns the package table must carry, in order. */
const COLUMNS = [
    'Package',
    'Kind',
    'Owns',
    'State',
    'Gate',
    'Owner docs',
    'Open slices',
    'Next milestone',
    'Notes',
];

/** States whose rows are outside every gate, and so may write `—` for `Gate`. */
const UNGATED_STATES = new Set(['review-only', 'excluded']);

/** Directories never treated as a package root, whatever they contain. */
const SKIP_ROOT_DIRS = new Set([
    '.git',
    '.github',
    'adr',
    'docs',
    'gradle',
    'images',
    'legacy-review',
    'locales',
    'mise-tasks',
    'node_modules',
    'rawshift',
    'target',
]);

/** An em-dash cell: "this column does not apply to this row". */
const NONE = '—';

/** Split one Markdown table row into trimmed cells, dropping the outer pipes. */
function cells(line) {
    const trimmed = line.trim().replace(/^\|/, '').replace(/\|$/, '');
    return trimmed.split('|').map((cell) => cell.trim());
}

/** True for a `| --- | --- |` separator row. */
function isSeparator(line) {
    return /^\|[\s:|-]+\|$/.test(line.trim());
}

/**
 * Every pipe table in `source`, as `{ header, rows }` where a row carries its
 * cells and the 1-based line it sits on.
 *
 * @param {string} source Markdown document.
 * @returns {{ header: string[], rows: { cells: string[], line: number }[] }[]}
 */
export function tables(source) {
    const lines = source.split('\n');
    const found = [];

    for (let i = 0; i < lines.length; i += 1) {
        if (!lines[i].trim().startsWith('|')) continue;
        if (!isSeparator(lines[i + 1] ?? '')) continue;

        const header = cells(lines[i]);
        const rows = [];
        let j = i + 2;
        for (; j < lines.length && lines[j].trim().startsWith('|'); j += 1) {
            rows.push({ cells: cells(lines[j]), line: j + 1 });
        }
        found.push({ header, rows });
        i = j;
    }

    return found;
}

/**
 * The state vocabulary the document defines, in document order.
 *
 * Definitions are the bullet list under the `## States` heading, each of the
 * form ``- `name` — meaning``.
 *
 * @param {string} source `ROADMAP.md`.
 * @returns {string[]}
 */
export function definedStates(source) {
    const opens = /^##\s+States\b.*$/m.exec(source);
    if (!opens) return [];
    const body = source.slice(opens.index + opens[0].length);
    const closes = /^##\s/m.exec(body);
    const section = closes ? body.slice(0, closes.index) : body;
    return [...section.matchAll(/^-\s+`([a-z][a-z-]*)`\s+—/gm)].map(
        (match) => match[1],
    );
}

/** Quoted strings on lines that are not comments. */
function quoted(text, pattern) {
    const found = [];
    for (const line of text.split('\n')) {
        const trimmed = line.trim();
        if (trimmed.startsWith('#') || trimmed.startsWith('//')) continue;
        for (const match of trimmed.matchAll(pattern)) found.push(match[1]);
    }
    return found;
}

/** `[workspace] members` — each member path is a cargo package row. */
function cargoPackages(root) {
    const manifest = join(root, 'Cargo.toml');
    if (!existsSync(manifest)) return [];
    const body = readFileSync(manifest, 'utf8');
    // `^members` anchors at line start, which `default-members` cannot match.
    const block = /^members\s*=\s*\[([\s\S]*?)^\]/m.exec(body);
    if (!block) return [];
    return quoted(block[1], /"([^"]+)"/g);
}

/** Unconditional `include(":x")`, resolved to the directory `project()` names. */
function gradlePackages(root) {
    const settings = join(root, 'settings.gradle.kts');
    if (!existsSync(settings)) return [];
    const body = readFileSync(settings, 'utf8');
    const included = quoted(body, /^include\("([^"]+)"\)/g);
    const dirs = new Map();
    for (const line of body.split('\n')) {
        const trimmed = line.trim();
        if (trimmed.startsWith('//')) continue;
        const match =
            /^project\("([^"]+)"\)\.projectDir\s*=\s*file\("([^"]+)"\)/.exec(
                trimmed,
            );
        if (match) dirs.set(match[1], match[2]);
    }
    return included.map((path) => dirs.get(path) ?? path.replace(/^:/, ''));
}

/** `module("Name"` framework targets plus the single app target. */
function tuistPackages(root) {
    const project = join(root, 'capsule-swift', 'Project.swift');
    if (!existsSync(project)) return [];
    const body = readFileSync(project, 'utf8');
    const names = [...body.matchAll(/\bmodule\(\s*"([A-Za-z0-9_]+)"/g)].map(
        (match) => match[1],
    );
    const app =
        /\bappTarget\s*:\s*Target\s*=\s*\.target\(\s*name:\s*"([A-Za-z0-9_]+)"/.exec(
            body,
        );
    if (app) names.push(app[1]);
    return names;
}

/** Root-level directories carrying `manifest`, e.g. `package.json`. */
function manifestDirs(root, manifest) {
    return readdirSync(root, { withFileTypes: true })
        .filter(
            (entry) =>
                entry.isDirectory() &&
                !entry.name.startsWith('.') &&
                !SKIP_ROOT_DIRS.has(entry.name) &&
                existsSync(join(root, entry.name, manifest)),
        )
        .map((entry) => entry.name);
}

/** `path = x` in `.gitmodules`. */
function submodules(root) {
    const modules = join(root, '.gitmodules');
    if (!existsSync(modules)) return [];
    return [
        ...readFileSync(modules, 'utf8').matchAll(/^\s*path\s*=\s*(\S+)/gm),
    ].map((match) => match[1]);
}

/** Each `legacy-review/<bucket>/` directory. */
function reviewBuckets(root) {
    const bucket = join(root, 'legacy-review');
    if (!existsSync(bucket)) return [];
    return readdirSync(bucket, { withFileTypes: true })
        .filter((entry) => entry.isDirectory())
        .map((entry) => `legacy-review/${entry.name}`);
}

/**
 * Every package the tree declares, as `name -> kind`.
 *
 * @param {string} root Repository root.
 * @returns {Map<string, string>}
 */
export function declaredPackages(root) {
    const declared = new Map();
    const add = (kind) => (name) => declared.set(name, kind);

    cargoPackages(root).forEach(add('cargo'));
    gradlePackages(root).forEach(add('gradle'));
    tuistPackages(root).forEach(add('tuist'));
    manifestDirs(root, 'Package.swift').forEach(add('swiftpm'));
    manifestDirs(root, 'package.json').forEach(add('bun'));
    manifestDirs(root, 'pyproject.toml').forEach(add('python'));
    if (existsSync(join(root, 'locales'))) declared.set('locales', 'catalog');
    submodules(root).forEach(add('submodule'));
    reviewBuckets(root).forEach(add('review-bucket'));

    return declared;
}

/** Every `mise run <task>` name the repository actually has. */
export function miseTasks(root) {
    const tasks = new Set();

    for (const manifest of ['mise.toml', 'capsule-swift/mise.toml']) {
        const path = join(root, manifest);
        if (!existsSync(path)) continue;
        for (const match of readFileSync(path, 'utf8').matchAll(
            /^\[tasks\.([A-Za-z0-9_-]+)\]/gm,
        )) {
            tasks.add(match[1]);
        }
    }

    const fileTasks = join(root, 'mise-tasks');
    if (existsSync(fileTasks)) {
        for (const entry of readdirSync(fileTasks, { withFileTypes: true })) {
            if (entry.isFile()) tasks.add(entry.name);
        }
    }

    return tasks;
}

/** Every slice id `SLICES.md` gives a detail block. */
export function sliceIds(root) {
    const path = join(root, SLICES);
    if (!existsSync(path)) return new Set();
    return new Set(
        [
            ...readFileSync(path, 'utf8').matchAll(/^###\s+(S-[A-Z]+\d+)\s/gm),
        ].map((match) => match[1]),
    );
}

/**
 * Resolve every `ROADMAP.md` row against the tree.
 *
 * @param {string} root Repository root.
 * @returns {{ findings: string[], checked: number }}
 */
export function checkRoadmap(root) {
    const findings = [];
    const path = join(root, ROADMAP);

    if (!existsSync(path)) {
        return {
            findings: [`${ROADMAP}  the package view is missing`],
            checked: 0,
        };
    }

    const source = readFileSync(path, 'utf8');
    const states = definedStates(source);
    if (states.length === 0) {
        findings.push(`${ROADMAP}  no state vocabulary under \`## States\``);
    }

    const parsed = tables(source);
    const table = parsed.find((candidate) => candidate.header[0] === 'Package');
    if (!table) {
        findings.push(
            `${ROADMAP}  no package table (its first column is \`Package\`)`,
        );
        return { findings, checked: 0 };
    }

    if (table.header.join(' | ') !== COLUMNS.join(' | ')) {
        findings.push(
            `${ROADMAP}  header is \`${table.header.join(' | ')}\`, expected \`${COLUMNS.join(' | ')}\``,
        );
    }

    const declared = declaredPackages(root);
    const tasks = miseTasks(root);
    const slices = sliceIds(root);
    const known = new Set(states);
    // Every state used anywhere in the document, so the deferred register below
    // the package table counts as a user of `deferred`.
    const used = new Set();
    for (const candidate of parsed) {
        const column = candidate.header.indexOf('State');
        if (column === -1) continue;
        for (const row of candidate.rows) {
            if (row.cells[column])
                used.add(row.cells[column].replace(/`/g, ''));
        }
    }

    const seen = new Set();

    for (const { cells: row, line } of table.rows) {
        const at = `${ROADMAP}:${line}`;

        if (row.length !== COLUMNS.length) {
            findings.push(
                `${at}  ${row.length} column(s), expected ${COLUMNS.length}`,
            );
            continue;
        }

        const [pkg, kind, , state, gate, , open] = row.map((cell) =>
            cell.replace(/`/g, ''),
        );

        if (seen.has(pkg)) findings.push(`${at}  ${pkg} has more than one row`);
        seen.add(pkg);

        const kindOf = declared.get(pkg);
        if (kindOf === undefined) {
            findings.push(
                `${at}  ${pkg} is not declared by any manifest in the tree`,
            );
        } else if (kindOf !== kind) {
            findings.push(
                `${at}  ${pkg} is declared as \`${kindOf}\`, the row says \`${kind}\``,
            );
        }

        if (!known.has(state)) {
            findings.push(
                `${at}  state \`${state}\` is not one of ${[...known].map((s) => `\`${s}\``).join(', ')}`,
            );
        }

        if (gate === NONE) {
            if (!UNGATED_STATES.has(state)) {
                findings.push(
                    `${at}  only ${[...UNGATED_STATES].join('/')} rows may leave \`Gate\` empty`,
                );
            }
        } else {
            const task = /^mise run ([A-Za-z0-9_-]+)$/.exec(gate);
            if (!task) {
                findings.push(
                    `${at}  gate \`${gate}\` is not \`mise run <task>\` or \`${NONE}\``,
                );
            } else if (!tasks.has(task[1])) {
                findings.push(
                    `${at}  \`mise run ${task[1]}\` is not a task in this repository`,
                );
            }
        }

        if (open !== NONE) {
            for (const id of open.split(',').map((entry) => entry.trim())) {
                if (!/^S-[A-Z]+\d+$/.test(id)) {
                    findings.push(`${at}  \`${id}\` is not a slice id`);
                } else if (!slices.has(id)) {
                    findings.push(
                        `${at}  \`${id}\` has no detail block in ${SLICES}`,
                    );
                }
            }
        }
    }

    for (const [pkg, kind] of declared) {
        if (!seen.has(pkg)) {
            findings.push(`${ROADMAP}  ${kind} package \`${pkg}\` has no row`);
        }
    }

    // A defined state nothing uses is reported and does **not** fail. Failing on
    // it would put steady pressure on whoever is editing the file to give the
    // spare term a row — which is a dishonest state assignment, the exact defect
    // this whole check exists to prevent. Naming it in the success line keeps the
    // vocabulary from rotting unnoticed at no such cost.
    const unused = [...known].filter((state) => !used.has(state));

    return { findings, checked: declared.size, unused };
}

/** @param {{ findings: string[], checked: number, unused?: string[] }} result */
export function reportRoadmap({ findings, checked, unused = [] }) {
    const spare =
        unused.length === 0
            ? ''
            : ` States defined and unused: ${unused.map((state) => `\`${state}\``).join(', ')}.`;
    if (findings.length === 0) {
        return `roadmap: ${checked} package(s) checked, all rows resolve.${spare}`;
    }
    return [
        `roadmap: ${findings.length} row(s) in ${ROADMAP} disagree with the tree.`,
        '',
        ...findings.map((finding) => `  ${finding}`),
        '',
        `roadmap failed: ${findings.length} unresolved of ${checked} package(s) checked.${spare}`,
    ].join('\n');
}

export const roadmapCheck = {
    name: 'roadmap',
    run: checkRoadmap,
    report: reportRoadmap,
};
