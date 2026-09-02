#!/usr/bin/env node

/**
 * docs-truth — the checks that keep the documentation's *names* real.
 *
 * Scope, stated once so it is not oversold: every check here proves that a name
 * a document uses resolves to something that exists — a file, an anchor, a
 * route. None of them proves that a document's description of a mechanism is
 * correct. That distinction is the whole contract; a reader who expects more
 * will disbelieve these gates the first time one misses.
 *
 * These checks deliberately live outside both `check-rust` and `check-docs`:
 *
 *   - `check-rust`'s CI paths filter excludes `capsule-docs/**`, so a docs-only
 *     pull request would skip them entirely. Widening that filter makes every
 *     `docs(design):` commit pay for a full cargo build.
 *   - `check-docs`'s expensive step is `astro build`. Adding the crate source
 *     trees to its filter makes a Rust rename rebuild the whole site.
 *
 * They regenerate nothing and need no toolchain — only `node:` builtins — which
 * is why the CI job that runs them installs no dependencies. That is deliberate,
 * not an omission. It also keeps them clear of the rule in
 * `design/developer-docs.md` that freshness gates belong to the owning
 * toolchain: that rule governs regenerating an artifact, and these checks
 * cross-reference committed text against committed text.
 *
 * That last claim is what excludes the generated `/reference/` pages, which
 * `scripts/lib/walk.mjs` prunes by path: they are gitignored build output, so
 * they exist on a machine that has built the site and on no CI runner, and a
 * check that read them would answer differently in the two places.
 *
 * Usage: `bun capsule-docs/scripts/docs-truth.mjs` from the repository root,
 * or `mise run check-docs-truth`.
 */

import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { crossLinksCheck } from './check-cross-links.mjs';
import { endpointCensusCheck } from './check-endpoint-census.mjs';
import { modulePathsCheck } from './check-module-paths.mjs';

/** Registered checks, run in order. Adding one is a single entry here. */
const CHECKS = [crossLinksCheck, endpointCensusCheck, modulePathsCheck];

function main() {
    // scripts/ -> capsule-docs/ -> repo root
    const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

    let failed = 0;
    const reports = [];

    for (const check of CHECKS) {
        const result = check.run(root);
        reports.push(check.report(result));
        if (result.findings.length > 0) failed += 1;
    }

    process.stdout.write(`${reports.join('\n\n')}\n`);

    if (failed > 0) {
        process.stdout.write(
            `\ndocs-truth: ${failed} of ${CHECKS.length} check(s) failed.\n`,
        );
        process.exitCode = 1;
    }
}

main();
