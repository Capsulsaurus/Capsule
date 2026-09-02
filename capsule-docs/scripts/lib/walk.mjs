/**
 * Filesystem walk shared by the docs-truth checks.
 *
 * Two rules here are load-bearing rather than incidental:
 *
 *   1. Symlinks are never followed. The repository root carries `docs ->
 *      capsule-docs/src/content/docs`, so a following walk visits every design
 *      doc twice and reports every finding twice. `readdir` with `withFileTypes`
 *      reports the link itself, which `isDirectory()`/`isFile()` both reject.
 *
 *   2. `rawshift/` is a git submodule with its own toolchain and CI, and CI does
 *      not check it out. A walk that descends into it passes on the runner and
 *      fails on any machine that has run `git submodule update` — the same trap
 *      `.markdownlint-cli2.jsonc` documents for its own ignore list.
 *
 *   3. The generated `/reference/` pages are the same trap in the other
 *      direction: gitignored build output that exists on any machine that has
 *      run `mise run build-docs` and on no CI runner, sitting *inside* the
 *      content tree every check scopes itself to. Walking them makes a check's
 *      verdict depend on whether the site happens to be built, which is exactly
 *      the property `docs-truth.mjs` claims not to have when it states its scope
 *      as committed text against committed text. They are pruned by path rather
 *      than by basename because `cli/` and `api/` are ordinary directory names
 *      that other trees are entitled to use.
 */

import { readdirSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

/** Directories never descended into, matched on their basename. */
const SKIP_DIRS = new Set([
    '.git',
    '.astro',
    '.build',
    '.venv',
    'Derived',
    'dist',
    'node_modules',
    'rawshift',
    'target',
]);

/**
 * Directory subtrees never descended into, matched on the repo-relative path.
 *
 * Build output that lands inside a scanned tree, so a basename rule cannot express it.
 */
const SKIP_PREFIXES = [
    'capsule-docs/src/content/docs/reference/cli/',
    'capsule-docs/src/content/docs/reference/api/',
];

/**
 * Yield repo-relative paths of every file under `root` whose name matches
 * `predicate`, depth-first, with `SKIP_DIRS` pruned and symlinks skipped.
 *
 * @param {string} root Absolute path to walk.
 * @param {(relPath: string) => boolean} predicate Tested against the repo-relative path.
 * @returns {string[]} Repo-relative paths, using `/` on every platform.
 */
export function walkFiles(root, predicate) {
    const found = [];

    const visit = (absDir) => {
        for (const entry of readdirSync(absDir, { withFileTypes: true })) {
            const abs = join(absDir, entry.name);
            // `isDirectory()`/`isFile()` are false for a symlink, which is how
            // the `docs` symlink is dropped without a special case for it.
            if (entry.isDirectory()) {
                const relDir = `${relative(root, abs).split(sep).join('/')}/`;
                if (
                    !SKIP_DIRS.has(entry.name) &&
                    !SKIP_PREFIXES.includes(relDir)
                ) {
                    visit(abs);
                }
                continue;
            }
            if (!entry.isFile()) continue;
            const rel = relative(root, abs).split(sep).join('/');
            if (predicate(rel)) found.push(rel);
        }
    };

    visit(root);
    return found.sort();
}
