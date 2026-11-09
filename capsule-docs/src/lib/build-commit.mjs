// The commit a build of the site was made from, shown in the footer. Read once
// at config time by astro.config.mjs. Hosted builds name the commit in the
// environment (the site deploys to Cloudflare Pages; see `deploy` in
// package.json); a local build asks git, so a build from a dirty tree names
// its HEAD. Pure apart from the injected reader.

const SHA = /^[0-9a-f]{7,40}$/i;

/**
 * Environment variables that name the built commit, in priority order.
 * Cloudflare Pages sets the first, GitHub Actions the second.
 */
export const COMMIT_ENV_VARS = ['CF_PAGES_COMMIT_SHA', 'GITHUB_SHA'];

/**
 * @typedef {object} BuildCommit
 * @property {string} sha Full hash, lower case.
 * @property {string} short First seven characters.
 */

/**
 * Resolves the built commit, or `null` when no source names a valid hash.
 *
 * @param {Record<string, string | undefined>} env
 * @param {() => string | null} readGitHead Returns `git rev-parse HEAD`'s
 *   output, or `null` when git is unavailable.
 * @returns {BuildCommit | null}
 */
export function resolveBuildCommit(env, readGitHead) {
    for (const name of COMMIT_ENV_VARS) {
        const commit = parseSha(env[name]);
        if (commit) return commit;
    }
    return parseSha(readGitHead());
}

/**
 * @param {string | null | undefined} value
 * @returns {BuildCommit | null}
 */
function parseSha(value) {
    const sha = value?.trim().toLowerCase();
    if (!sha || !SHA.test(sha)) return null;
    return { sha, short: sha.slice(0, 7) };
}
