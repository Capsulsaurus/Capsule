#!/usr/bin/env bun
// Stamp docs pages as audited: pin each to the current commit, today's UTC
// date and a digest of its content (src/lib/page-audit.mjs). The page then
// computes to `audited` until it next changes.
//
// The pinned commit must hold exactly what was audited, so a page whose
// content differs from HEAD (beyond its audit field) is refused: commit the
// edits, audit the page as committed, stamp it, then commit the stamp. Every
// page is checked before any is written, so a refusal leaves all of them as
// they were.
//
// Usage: bun run audit-page <page.md>...   (from capsule-docs/)
//        mise run audit-docs -- <page.md>... (from the repository root)
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
    auditDigest,
    parseAudit,
    stampAudit,
    stripAuditBlock,
} from '../src/lib/page-audit.mjs';
import { frontmatterGenerated } from './lib/markdown.mjs';

/**
 * @typedef {{
 *   git: (args: string[]) => string,
 *   read: (path: string) => string,
 *   write: (path: string, text: string) => void,
 *   now: () => Date,
 *   log: (line: string) => void,
 * }} Io
 */

/** @type {Io} */
const REAL_IO = {
    // Raw output: `git show` must reach the comparison byte for byte.
    git: (args) =>
        execFileSync('git', args, {
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'pipe'],
        }),
    read: (path) => readFileSync(path, 'utf8'),
    write: (path, text) => writeFileSync(path, text),
    now: () => new Date(),
    log: (line) => process.stdout.write(`${line}\n`),
};

/**
 * Stamp `files`, or throw listing every page that cannot be stamped, having
 * written none.
 *
 * @param {string[]} files Paths relative to the working directory.
 * @param {Io} [io]
 * @returns {{ file: string, commit: string, date: string, digest: string, previous: string }[]}
 */
export function auditPages(files, io = REAL_IO) {
    if (files.length === 0) throw new Error('usage: audit-page <page.md>...');
    const commit = io.git(['rev-parse', 'HEAD']).trim();
    const root = io.git(['rev-parse', '--show-toplevel']).trim();
    const date = io.now().toISOString().slice(0, 10);
    io.log(
        JSON.stringify({
            event: 'audit.start',
            commit,
            date,
            files: files.length,
        }),
    );

    const problems = [];
    const stamps = [];
    for (const file of files) {
        const path = resolve(file);
        const source = io.read(path);
        if (frontmatterGenerated(source)) {
            problems.push(`${file}: a generated page has no audit`);
            continue;
        }
        const tracked = relative(root, path).split('\\').join('/');
        let committed;
        try {
            committed = io.git(['show', `HEAD:${tracked}`]);
        } catch {
            problems.push(
                `${file}: not committed at HEAD; commit it before stamping`,
            );
            continue;
        }
        // Exact: the digest is of these bytes, so the commit must hold them.
        if (stripAuditBlock(committed) !== stripAuditBlock(source)) {
            problems.push(
                `${file}: differs from HEAD; commit the edits before stamping`,
            );
            continue;
        }
        // Stamp in memory here, so a page stampAudit cannot stamp (no YAML
        // frontmatter, say) is refused before any page is written.
        let stamped;
        try {
            stamped = stampAudit(source, { commit, date });
        } catch (error) {
            problems.push(
                `${file}: ${error instanceof Error ? error.message : error}`,
            );
            continue;
        }
        stamps.push({ file, path, source, stamped });
    }
    if (problems.length > 0) {
        for (const problem of problems) {
            io.log(JSON.stringify({ event: 'audit.refused', reason: problem }));
        }
        throw new Error(`nothing stamped:\n${problems.join('\n')}`);
    }

    return stamps.map(({ file, path, source, stamped }) => {
        const previous = parseAudit(source).state;
        io.write(path, stamped);
        const record = {
            file,
            commit,
            date,
            digest: auditDigest(source),
            previous,
        };
        io.log(JSON.stringify({ event: 'audit.stamped', ...record }));
        return record;
    });
}

if (
    process.argv[1] &&
    resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
    try {
        auditPages(process.argv.slice(2));
    } catch (error) {
        process.stderr.write(
            `${error instanceof Error ? error.message : error}\n`,
        );
        process.exit(1);
    }
}
