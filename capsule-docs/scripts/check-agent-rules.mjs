/**
 * Scoped agent-rule check.
 *
 * `AGENTS.md` carries only what applies everywhere; everything that governs one
 * area lives in a markdown file under `.claude/rules/` whose `paths:` front
 * matter says when it loads, and which names the docs a change there must
 * follow or keep current. Both halves of that arrangement fail silently:
 *
 *   - A glob that stops matching (a crate renamed, a directory moved) means
 *     the rule never loads again, and nothing anywhere says so.
 *   - A new component directory with no rule scoped to it means nobody is told
 *     which docs it answers to.
 *
 * So this check proves four things, all names against the tracked tree:
 *
 *   1. every rule file declares `paths:`;
 *   2. every glob matches at least one tracked file;
 *   3. every top-level directory carrying a build manifest has a rule scoped to
 *      the whole of it (`<dir>/**`) — a repo-wide `**\/*.rs`, or another area's
 *      `<dir>/src/**\/*.rs`, is not coverage, because the point of a scoped
 *      rule is the directory-specific doc map;
 *   4. the `AGENTS.md` index links exactly the rule files that exist, so a
 *      reader of `AGENTS.md` alone (any agent that does not load
 *      `.claude/rules/`) can find every rule.
 *
 * Like the rest of docs-truth it proves names resolve, never that a rule's
 * prose — or the bare module names in its doc-map tables — is right. Link
 * targets inside rule files are `check-cross-links`'s; `crate::path` citations
 * are `check-module-paths`'s.
 */

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { join, matchesGlob } from 'node:path';
import { walkFiles } from './lib/walk.mjs';

/** Where the scoped rules live, relative to the repo root. */
const RULES_DIR = '.claude/rules';

/** The always-loaded instruction file that indexes them. */
const INDEX = 'AGENTS.md';

/** A file at a top-level directory's root that makes it a component. */
const MANIFESTS = new Set([
    'Cargo.toml',
    'package.json',
    'build.gradle.kts',
    'Package.swift',
    'Project.swift',
]);

/**
 * Top-level directories with a manifest that need no rule of their own.
 * `legacy-review/` is non-buildable reference material; `rawshift/` is a
 * submodule with its own instructions.
 */
const EXEMPT_DIRS = new Set(['legacy-review', 'rawshift']);

/** Strip a trailing YAML comment and one pair of matching quotes. */
function scalar(value) {
    return value
        .replace(/\s+#.*$/, '')
        .trim()
        .replace(/^(["'])(.*)\1$/, '$2');
}

/**
 * Split an inline YAML list body on the commas that separate items, not the
 * ones inside a brace glob (`"a/{x,y}/**"`) or a quoted string.
 *
 * @param {string} body
 * @returns {string[]}
 */
function splitInline(body) {
    const items = [];
    let depth = 0;
    let quote = null;
    let current = '';
    for (const ch of body) {
        if (quote) {
            if (ch === quote) quote = null;
        } else if (ch === '"' || ch === "'") {
            quote = ch;
        } else if (ch === '{') {
            depth += 1;
        } else if (ch === '}') {
            depth -= 1;
        } else if (ch === ',' && depth === 0) {
            items.push(current);
            current = '';
            continue;
        }
        current += ch;
    }
    items.push(current);
    return items;
}

/**
 * The `paths:` globs from a rule file's YAML front matter, or null when there
 * is no front matter or no `paths` key. Reads the block-list form (indented or
 * not, with blank and comment lines inside it) and the inline `[a, b]` form.
 *
 * @param {string} text
 * @returns {string[] | null}
 */
export function parseRulePaths(text) {
    const front = /^﻿?---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(text);
    if (!front) return null;

    const lines = front[1].split(/\r?\n/);
    const start = lines.findIndex((line) => /^paths:/.test(line));
    if (start === -1) return null;

    const inline = /^paths:\s*\[(.*)\]\s*$/.exec(lines[start]);
    if (inline) return splitInline(inline[1]).map(scalar).filter(Boolean);

    const globs = [];
    for (const line of lines.slice(start + 1)) {
        if (/^\s*(#.*)?$/.test(line)) continue;
        const item = /^\s*-\s+(.+?)\s*$/.exec(line);
        if (!item) break;
        globs.push(scalar(item[1]));
    }
    return globs;
}

/**
 * Repo-relative paths of every tracked file that is still on disk, so a glob
 * that only matches build output or an uncommitted file fails here as it would
 * on a CI checkout.
 *
 * A filesystem walk stands in only when `root` is not a git work tree at all,
 * which is where the unit tests' fixtures live. Inside one, a failing `git`
 * (missing, or refusing a "dubious ownership" checkout) is an error: falling
 * back there would quietly count untracked files and pass what CI fails.
 *
 * @param {string} root
 * @returns {string[]}
 */
export function trackedFiles(root) {
    if (!existsSync(join(root, '.git'))) return walkFiles(root, () => true);
    const listed = execFileSync('git', ['ls-files', '-z'], {
        cwd: root,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'inherit'],
        maxBuffer: 64 * 1024 * 1024,
    });
    return listed
        .split('\0')
        .filter((rel) => rel !== '' && existsSync(join(root, rel)));
}

/**
 * @param {string} root Absolute repository root.
 * @param {string[]} [files] Repo-relative file list; defaults to the tracked tree.
 * @returns {{ findings: string[], checked: number }}
 */
export function checkAgentRules(root, files = trackedFiles(root)) {
    const findings = [];
    const ruleFiles = files
        .filter(
            (file) => file.startsWith(`${RULES_DIR}/`) && file.endsWith('.md'),
        )
        .sort();

    /** Every glob any rule declares. */
    const globs = [];
    for (const ruleFile of ruleFiles) {
        const paths = parseRulePaths(
            readFileSync(join(root, ruleFile), 'utf8'),
        );
        if (paths === null || paths.length === 0) {
            findings.push(
                `${ruleFile}  no \`paths:\` front matter; it would load in every session`,
            );
            continue;
        }
        for (const glob of paths) {
            globs.push(glob);
            if (!files.some((file) => matchesGlob(file, glob))) {
                findings.push(
                    `${ruleFile}  ${glob}  matches no tracked file; the rule never loads for it`,
                );
            }
        }
    }

    // Components: a manifest directly under a top-level directory.
    const components = new Set();
    for (const file of files) {
        const parts = file.split('/');
        if (
            parts.length === 2 &&
            MANIFESTS.has(parts[1]) &&
            !EXEMPT_DIRS.has(parts[0])
        ) {
            components.add(parts[0]);
        }
    }
    for (const dir of [...components].sort()) {
        if (!globs.includes(`${dir}/**`)) {
            findings.push(
                `${dir}/  no rule in ${RULES_DIR}/ is scoped to \`${dir}/**\``,
            );
        }
    }

    // The index links exactly the rule files that exist.
    const indexAbs = join(root, INDEX);
    const indexed = new Set();
    if (existsSync(indexAbs)) {
        const link = /\]\(\.claude\/rules\/([A-Za-z0-9_./-]+\.md)\)/g;
        for (const match of readFileSync(indexAbs, 'utf8').matchAll(link)) {
            indexed.add(`${RULES_DIR}/${match[1]}`);
        }
    }
    for (const ruleFile of ruleFiles) {
        if (!indexed.has(ruleFile)) {
            findings.push(`${ruleFile}  not linked from the ${INDEX} index`);
        }
    }
    for (const entry of [...indexed].sort()) {
        if (!ruleFiles.includes(entry)) {
            findings.push(`${INDEX}  indexes ${entry}, which does not exist`);
        }
    }

    return { findings, checked: ruleFiles.length };
}

/** @param {{ findings: string[], checked: number }} result */
export function reportAgentRules({ findings, checked }) {
    if (findings.length === 0) {
        return `agent-rules: ${checked} rule file(s) checked, all scoped, live and indexed.`;
    }
    return [
        `agent-rules: ${findings.length} problem(s) with ${RULES_DIR}/.`,
        '',
        ...findings.map((f) => `  ${f}`),
        '',
        `agent-rules failed: ${findings.length} finding(s) across ${checked} rule file(s).`,
    ].join('\n');
}

export const agentRulesCheck = {
    name: 'agent-rules',
    run: (root) => checkAgentRules(root),
    report: reportAgentRules,
};
