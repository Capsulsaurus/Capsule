/**
 * Module and type path resolution.
 *
 * A design doc's value is that a reader can grep for what it names. When it
 * names `capsule-server::federation` and no such module exists, the doc has
 * stopped being a map and started being a story.
 *
 * **Scope is `capsule-docs/` and `AGENTS.md` only, deliberately not
 * `SLICES.md`.** The slice tracker is a historical ledger: it records that
 * `capsule_core::media` *was* retired and that `capsule-api` *used to* hold the
 * upload module. Forcing it to name only live modules would corrupt the record.
 *
 * Resolution is strict on the way down and lenient at the leaf, which is what
 * takes the false-positive rate to zero on the real corpus:
 *
 *   - Each non-final segment must be a real directory or `<segment>.rs`.
 *   - The final segment may instead be an *item* — `pub fn`, `pub struct`, a
 *     `pub use` re-export — found anywhere in the resolved module's subtree.
 *     `capsule-core::library::available_bytes` is a `pub use` in
 *     `library/mod.rs`, not a file, and a resolver that demands a file reports
 *     it as missing.
 *
 * A path that resolves one module too shallow is accepted. That is the right
 * trade: the failure worth catching is "names something that does not exist",
 * not "names it at the wrong depth", and tightening it reintroduces the
 * re-export false positive above.
 */

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { walkFiles } from './lib/walk.mjs';

/** Files scanned for citations. */
const SCOPE = ['capsule-docs/src/content/docs/', 'AGENTS.md'];

/**
 * Modules the design specifies and the tree does not have yet, as
 * `<path>\t<reason>`. Unlike the endpoint allowlist this is keyed on the path
 * alone: a module that does not exist does not exist in every doc that names it.
 *
 * This file is also the answer to "what has the design committed to that
 * nobody has built?" for the module layer, which is why each row carries its
 * slice rather than a bare exemption.
 */
const PLANNED = 'capsule-docs/planned-modules.txt';

/** `capsule-core::crypto::keys`, `capsule_sdk::rest`, `capsule-core::{library,db}`. */
const PATH_PATTERN =
    /`(capsule[-_][a-z][a-z-]*(?:::(?:\{[^}]*\}|[A-Za-z_][A-Za-z0-9_]*))+)`/g;

/** Every `.rs` file under `dir`, recursively. */
function rustFiles(dir) {
    const found = [];
    const visit = (current) => {
        for (const entry of readdirSync(current, { withFileTypes: true })) {
            const abs = join(current, entry.name);
            if (entry.isDirectory()) visit(abs);
            else if (entry.isFile() && entry.name.endsWith('.rs'))
                found.push(abs);
        }
    };
    visit(dir);
    return found;
}

/** True when any `.rs` file under `dir` declares or re-exports `name`. */
function declaresItem(dir, name) {
    const declaration = new RegExp(
        `\\bpub(?:\\s*\\([^)]*\\))?\\s+(?:unsafe\\s+|async\\s+|const\\s+|extern\\s+"[^"]*"\\s+)*` +
            `(?:fn|struct|enum|trait|type|const|static|union|mod)\\s+${name}\\b`,
    );
    const reexport = new RegExp(
        `\\bpub\\s+use\\b[^;]*\\b${name}\\b[^;]*;`,
        's',
    );
    for (const file of rustFiles(dir)) {
        const body = readFileSync(file, 'utf8');
        if (declaration.test(body) || reexport.test(body)) return true;
    }
    return false;
}

/** Expand `a::{b,c}` into `a::b`, `a::c`. */
function expand(path) {
    const brace = /\{([^}]*)\}/.exec(path);
    if (!brace) return [path];
    return brace[1]
        .split(',')
        .map((part) => part.trim())
        .filter(Boolean)
        .map(
            (part) =>
                path.slice(0, brace.index) +
                part +
                path.slice(brace.index + brace[0].length),
        );
}

/**
 * Resolve one path. Returns null when it resolves, or a reason when it does not.
 *
 * @param {string} root
 * @param {string} path e.g. `capsule-core::crypto::keys`
 * @returns {string | null}
 */
export function resolveModulePath(root, path) {
    const [crateSegment, ...segments] = path.split('::');
    const crate = crateSegment.replace(/_/g, '-');
    const crateSrc = join(root, crate, 'src');
    if (!existsSync(crateSrc)) {
        return `no crate \`${crate}\` with a src/ directory`;
    }

    let dir = crateSrc;
    for (const [index, segment] of segments.entries()) {
        const asDir = join(dir, segment);
        const asFile = join(dir, `${segment}.rs`);
        const isDir = existsSync(asDir) && statSync(asDir).isDirectory();
        const isFile = existsSync(asFile);

        if (isDir) {
            dir = asDir;
            continue;
        }
        if (isFile) {
            if (index === segments.length - 1) return null;
            // A leaf file can still contain the remaining segments as items.
            return declaresItem(dir, segments[segments.length - 1])
                ? null
                : `no \`${segments.slice(index).join('::')}\` in ${crate}/src/${segments.slice(0, index).join('/')}`;
        }
        if (index === segments.length - 1 && declaresItem(dir, segment)) {
            return null;
        }
        const where = segments.slice(0, index).join('/');
        const siblings = readdirSync(dir, { withFileTypes: true })
            .filter(
                (entry) => entry.isDirectory() || entry.name.endsWith('.rs'),
            )
            .map((entry) => entry.name.replace(/\.rs$/, ''))
            .filter(
                (name) => name !== 'mod' && name !== 'lib' && name !== 'main',
            )
            .sort();
        return (
            `no \`${segment}\` in ${crate}/src${where ? `/${where}` : ''}` +
            (siblings.length > 0 && siblings.length <= 30
                ? ` (has: ${siblings.join(', ')})`
                : '')
        );
    }
    return null;
}

/**
 * @param {string} root Absolute repo root.
 * @returns {{ findings: string[], checked: number }}
 */
export function checkModulePaths(root) {
    const findings = [];
    const planned = new Set();
    const exercised = new Set();
    const plannedFile = join(root, PLANNED);
    if (existsSync(plannedFile)) {
        for (const line of readFileSync(plannedFile, 'utf8').split('\n')) {
            const row = line.trim();
            if (!row || row.startsWith('#')) continue;
            const [path] = row.split('\t');
            if (path) planned.add(path.trim());
        }
    }

    let checked = 0;
    const sources = walkFiles(root, (rel) =>
        SCOPE.some((prefix) =>
            prefix.endsWith('/') ? rel.startsWith(prefix) : rel === prefix,
        ),
    );

    for (const source of sources) {
        const body = readFileSync(join(root, source), 'utf8');
        for (const match of body.matchAll(PATH_PATTERN)) {
            const line = body.slice(0, match.index).split('\n').length;
            for (const path of expand(match[1])) {
                checked += 1;
                // A planned module covers everything beneath it: if
                // `capsule-core::media` does not exist, neither can
                // `capsule-core::media::video::derivative`.
                const cover = [...planned].find(
                    (entry) => path === entry || path.startsWith(`${entry}::`),
                );
                if (cover) {
                    exercised.add(cover);
                    continue;
                }
                const reason = resolveModulePath(root, path);
                if (reason)
                    findings.push(`${source}:${line}  ${path}  ${reason}`);
            }
        }
    }

    // A planned module that has since been built, or that no doc names any
    // more, should leave this list rather than sit in it unexamined.
    for (const path of planned) {
        if (exercised.has(path)) {
            if (resolveModulePath(root, path) === null) {
                findings.push(
                    `${PLANNED}  ${path} exists now; remove it from the planned list`,
                );
            }
        } else {
            findings.push(
                `${PLANNED}  no doc names ${path} any more; remove the stale entry`,
            );
        }
    }

    return { findings, checked };
}

/** @param {{ findings: string[], checked: number }} result */
export function reportModulePaths({ findings, checked }) {
    if (findings.length === 0) {
        return `module-paths: ${checked} path(s) checked, all resolve.`;
    }
    return [
        `module-paths: ${findings.length} citation(s) name Rust paths that do not exist.`,
        '',
        ...findings.map((f) => `  ${f}`),
        '',
        `module-paths failed: ${findings.length} unresolved of ${checked} checked.`,
    ].join('\n');
}

export const modulePathsCheck = {
    name: 'module-paths',
    run: checkModulePaths,
    report: reportModulePaths,
};
