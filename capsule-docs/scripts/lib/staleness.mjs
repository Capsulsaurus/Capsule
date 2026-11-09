/**
 * Whether a committed reference artifact may be stale on this branch.
 *
 * The authority is CI: each artifact's own gate regenerates it from the code and fails on any
 * byte difference, so `master` never holds a stale one. Between edits on a branch, though, a
 * developer can change a doc comment and run `astro dev` before regenerating, and the page
 * would show the old text without a word. This module catches that case from git alone, with
 * no toolchain: an artifact is *possibly stale* when its sources changed after it did.
 *
 * It is a heuristic and only ever warns. A source change that does not alter the surface (a
 * function body, a plain comment) flags the artifact until it is regenerated once: every
 * generator rewrites its file even when the bytes are unchanged, so its modification time then
 * passes the sources' and the warning clears. The CI gate, not this, is what a page's
 * correctness rests on.
 */

import { execFileSync } from 'node:child_process';
import { statSync } from 'node:fs';
import { join } from 'node:path';

/**
 * Each artifact, the repo-relative path prefixes its content is generated from, and the
 * command that regenerates it.
 */
export const ARTIFACT_SOURCES = {
    'capsule-server/openapi.json': {
        sources: ['capsule-server/src/', 'capsule-server/Cargo.toml'],
        regenerate: 'mise run openapi-kynos',
    },
    'capsule-cli/cli-surface.json': {
        sources: ['capsule-cli/src/'],
        regenerate: 'mise run cli-surface',
    },
    'capsule-sdk/bindings-surface.json': {
        sources: ['capsule-sdk/src/', 'capsule-sdk/Cargo.toml'],
        regenerate: 'mise run gen-bindings',
    },
    'capsule-wasm/wasm-surface.d.ts': {
        sources: ['capsule-wasm/src/', 'capsule-wasm/Cargo.toml'],
        regenerate: 'mise run wasm-surface',
    },
};

/**
 * @param {string} path
 * @param {string[]} prefixes
 * @returns {boolean}
 */
function touches(path, prefixes) {
    return prefixes.some(
        (prefix) => path === prefix || path.startsWith(prefix),
    );
}

/**
 * The artifacts whose sources changed after they did.
 *
 * - An uncommitted change to a source, with the artifact itself unchanged, is stale.
 * - An uncommitted change to the artifact is a fresh regeneration: not stale.
 * - Otherwise, over the branch's own commits (newest first), the artifact is stale when the
 *   newest commit touching a source is newer than the newest touching the artifact.
 * - Either way, with `mtimeOf`, it is stale only while some changed source on disk is newer
 *   than the artifact: regenerating rewrites the artifact, which clears it.
 *
 * @param {{ dirty: string[], commits: string[][] }} changes `dirty` is every uncommitted path;
 *   `commits` the paths each commit since the merge base touched, newest first.
 * @param {typeof ARTIFACT_SOURCES} [sources]
 * @param {(path: string) => number | undefined} [mtimeOf] A path's modification time, or
 *   `undefined` when it does not exist.
 * @returns {string[]} Stale artifact paths, in `sources` order.
 */
export function staleArtifacts(
    changes,
    sources = ARTIFACT_SOURCES,
    mtimeOf = undefined,
) {
    const stale = [];
    for (const [artifact, { sources: prefixes }] of Object.entries(sources)) {
        if (changes.dirty.includes(artifact)) continue;
        const changed = [...changes.dirty, ...changes.commits.flat()].filter(
            (path) => touches(path, prefixes),
        );
        const source = changes.commits.findIndex((paths) =>
            paths.some((path) => touches(path, prefixes)),
        );
        const regenerated = changes.commits.findIndex((paths) =>
            paths.includes(artifact),
        );
        const suspect =
            changes.dirty.some((path) => touches(path, prefixes)) ||
            (source !== -1 && (regenerated === -1 || source < regenerated));
        if (!suspect) continue;
        if (mtimeOf) {
            const built = mtimeOf(artifact) ?? 0;
            if (!changed.some((path) => (mtimeOf(path) ?? 0) > built)) continue;
        }
        stale.push(artifact);
    }
    return stale;
}

/**
 * A repo-relative path's modification time in milliseconds, or `undefined` when it is gone.
 *
 * @param {string} root
 * @returns {(path: string) => number | undefined}
 */
export function mtimes(root) {
    return (path) => {
        try {
            return statSync(join(root, path)).mtimeMs;
        } catch {
            return undefined;
        }
    };
}

/**
 * Why the check is skipped in this environment, or `null` to run it. CI and Cloudflare's
 * build have their own authority (the drift gates, and a `master` those gates protect).
 *
 * @param {Record<string, string | undefined>} env
 * @returns {string | null}
 */
export function skipReason(env) {
    if (env.CI) return 'CI, where the drift gates are the check';
    if (env.CF_PAGES)
        return 'a Cloudflare Pages build, which builds verified commits';
    return null;
}

/**
 * Read the branch's changes from git: uncommitted paths, and the paths each commit since the
 * merge base with `origin/master` touched. `null` when git or that ref is unavailable.
 *
 * @param {string} root Repository root.
 * @returns {{ dirty: string[], commits: string[][] } | null}
 */
export function readChanges(root) {
    const git = (...args) =>
        execFileSync('git', ['-C', root, ...args], {
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'ignore'],
        });
    try {
        const base = git('merge-base', 'HEAD', 'origin/master').trim();
        const dirty = git('status', '--porcelain=v1', '--untracked-files=all')
            .split('\n')
            .filter(Boolean)
            .map((line) => line.slice(3).split(' -> ').pop());
        const commits = git(
            'log',
            '--format=%x00',
            '--name-only',
            `${base}..HEAD`,
        )
            .split('\0')
            .slice(1)
            .map((block) => block.split('\n').filter(Boolean));
        return { dirty, commits };
    } catch {
        return null;
    }
}
