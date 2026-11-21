// The computed audit of a docs collection entry, for the site: the sidebar
// (src/route-data.ts) and the page title (src/components/PageTitle.astro) both
// call this, so the two can never disagree. The digest covers the raw file,
// so the file is read from disk rather than reconstructed from parsed data.

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pageAudit } from './page-audit.mjs';

/** The repository audit commits link into. */
export const REPOSITORY_URL = 'https://github.com/Capsulsaurus/Capsule';

/**
 * @param {{ id: string, filePath?: string, data: { audit?: unknown } }} entry
 * @param {(path: string) => string} [read] Reads a file; injected by tests.
 * @returns {import('./page-audit.mjs').PageAudit | undefined} Undefined for a
 *   page with no audit field: a generated page, or a plugin route with no file.
 */
export function entryAudit(entry, read = (path) => readFileSync(path, 'utf8')) {
    if (entry.data.audit === undefined || !entry.filePath) return undefined;
    return pageAudit(read(resolve(entry.filePath)), entry.id);
}

/** @param {string} commit */
export function commitUrl(commit) {
    return `${REPOSITORY_URL}/commit/${commit}`;
}
