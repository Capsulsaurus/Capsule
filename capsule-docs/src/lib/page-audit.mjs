// A docs page's audit pin and the status computed from it (see
// src/content.config.ts and Core Principles — Audit Status).
//
// A page's frontmatter carries either `audit: none` (never audited) or a stamp:
//
//     audit:
//       commit: <40-hex sha>      the commit whose copy of the page was audited
//       date: YYYY-MM-DD          when, in UTC
//       digest: sha256:<64-hex>   of the page with this block removed
//
// The status is never written down: it is computed by comparing the stamped
// digest with the page as it is now, so an edit after an audit shows at once
// and no git history is needed to tell. The schema, the page test, the
// stamping script and the site all read a page through this one module.

import { createHash } from 'node:crypto';

/** @typedef {'audited' | 'outdated' | 'unaudited'} AuditStatus */
/**
 * @typedef {{ state: 'absent' }
 *   | { state: 'none' }
 *   | { state: 'stamp', commit: string, date: string, digest: string }
 *   | { state: 'malformed', reason: string }} ParsedAudit
 */
/**
 * @typedef {{ status: 'unaudited' }
 *   | { status: 'audited' | 'outdated', commit: string, date: string }} PageAudit
 */

/** Every computed status, in display order. @type {readonly AuditStatus[]} */
export const AUDIT_STATUSES = Object.freeze([
    'audited',
    'outdated',
    'unaudited',
]);

export const COMMIT_PATTERN = /^[0-9a-f]{40}$/;
export const DATE_PATTERN = /^\d{4}-\d{2}-\d{2}$/;
export const DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/;

const FIELDS = /** @type {const} */ ({
    commit: COMMIT_PATTERN,
    date: DATE_PATTERN,
    digest: DIGEST_PATTERN,
});
// Fences may carry trailing blanks, as Astro's own frontmatter reader allows.
const FRONTMATTER = /^---[ \t]*\n([\s\S]*?)\n---[ \t]*(?=\n|$)/;
const AUDIT_KEY = /^audit:(.*)$/;
const INDENTED = /^[ \t]+\S/;

/** LF line endings and no byte-order mark. @param {string} source */
const normalize = (source) => source.replace(/^﻿/, '').replace(/\r\n/g, '\n');

/**
 * Whether `date` is a real YYYY-MM-DD calendar day (not 2026-02-30).
 *
 * @param {string} date
 */
export function isCalendarDate(date) {
    if (!DATE_PATTERN.test(date)) return false;
    const parsed = new Date(`${date}T00:00:00Z`);
    return (
        !Number.isNaN(parsed.getTime()) &&
        parsed.toISOString().slice(0, 10) === date
    );
}

/** A YAML scalar with its quotes and trailing comment removed. */
const scalar = (/** @type {string} */ raw) =>
    raw
        .replace(/\s+#.*$/, '')
        .trim()
        .replace(/^(['"])(.*)\1$/, '$2');

/**
 * Split a normalized page into its frontmatter lines and the text around them.
 *
 * @param {string} text
 */
function splitFrontmatter(text) {
    const match = FRONTMATTER.exec(text);
    if (!match) return undefined;
    return {
        lines: match[1].split('\n'),
        rest: text.slice(match[0].length),
    };
}

/**
 * The index range [start, end) of the audit field in frontmatter lines: the
 * `audit:` line and every indented line under it.
 *
 * @param {string[]} lines
 */
function auditRange(lines) {
    const start = lines.findIndex((line) => AUDIT_KEY.test(line));
    if (start === -1) return undefined;
    let end = start + 1;
    while (end < lines.length && INDENTED.test(lines[end])) end++;
    return { start, end };
}

/** @param {string[]} lines @param {string} rest */
const join = (lines, rest) => `---\n${lines.join('\n')}\n---${rest}`;

/**
 * The page with line endings normalized to LF and its frontmatter `audit`
 * field removed: exactly what an audit digest covers.
 *
 * @param {string} source
 * @returns {string}
 */
export function stripAuditBlock(source) {
    const text = normalize(source);
    const parts = splitFrontmatter(text);
    const range = parts && auditRange(parts.lines);
    if (!parts || !range) return text;
    const lines = parts.lines.toSpliced(range.start, range.end - range.start);
    return join(lines, parts.rest);
}

/**
 * The digest an audit stamp pins: sha256 of {@link stripAuditBlock}.
 *
 * @param {string} source
 * @returns {string}
 */
export function auditDigest(source) {
    const hash = createHash('sha256').update(stripAuditBlock(source));
    return `sha256:${hash.digest('hex')}`;
}

/**
 * Read a page's audit field. Only the two documented shapes are accepted;
 * anything else is `malformed` with the reason, never guessed at.
 *
 * @param {string} source
 * @returns {ParsedAudit}
 */
export function parseAudit(source) {
    const parts = splitFrontmatter(normalize(source));
    const range = parts && auditRange(parts.lines);
    if (!parts || !range) return { state: 'absent' };
    const lines = parts.lines.slice(range.start, range.end);
    const head = scalar(
        /** @type {RegExpExecArray} */ (AUDIT_KEY.exec(lines[0]))[1],
    );
    if (head === 'none' && lines.length === 1) return { state: 'none' };
    if (head !== '') {
        return {
            state: 'malformed',
            reason: `audit is "${head}", not none or a stamp`,
        };
    }
    /** @type {Record<string, string>} */
    const stamp = {};
    for (const line of lines.slice(1)) {
        const field = /^\s+([\w-]+):(.*)$/.exec(line);
        if (!field)
            return {
                state: 'malformed',
                reason: `unreadable line "${line.trim()}"`,
            };
        const [, key, raw] = field;
        if (!Object.hasOwn(FIELDS, key)) {
            return { state: 'malformed', reason: `unknown field audit.${key}` };
        }
        if (Object.hasOwn(stamp, key)) {
            return {
                state: 'malformed',
                reason: `repeated field audit.${key}`,
            };
        }
        stamp[key] = scalar(raw);
    }
    for (const [key, pattern] of Object.entries(FIELDS)) {
        if (!Object.hasOwn(stamp, key)) {
            return { state: 'malformed', reason: `missing field audit.${key}` };
        }
        if (!pattern.test(stamp[key])) {
            return {
                state: 'malformed',
                reason: `audit.${key} "${stamp[key]}" does not match ${pattern}`,
            };
        }
    }
    if (!isCalendarDate(stamp.date)) {
        return {
            state: 'malformed',
            reason: `audit.date "${stamp.date}" is not a calendar day`,
        };
    }
    return {
        state: 'stamp',
        commit: stamp.commit,
        date: stamp.date,
        digest: stamp.digest,
    };
}

/**
 * What a page's audit says about it now, or undefined when it has no audit
 * field (a generated or plugin page). Throws on a malformed field, naming the
 * page, so a broken stamp fails the build rather than showing a wrong badge.
 *
 * @param {string} source
 * @param {string} [page] Named in the error.
 * @returns {PageAudit | undefined}
 */
export function pageAudit(source, page = 'page') {
    const audit = parseAudit(source);
    switch (audit.state) {
        case 'absent':
            return undefined;
        case 'none':
            return { status: 'unaudited' };
        case 'malformed':
            throw new Error(`${page}: malformed audit: ${audit.reason}`);
        case 'stamp':
            return {
                status:
                    audit.digest === auditDigest(source)
                        ? 'audited'
                        : 'outdated',
                commit: audit.commit,
                date: audit.date,
            };
    }
}

/**
 * Just the computed status of {@link pageAudit}.
 *
 * @param {string} source
 * @returns {AuditStatus | undefined}
 */
export function auditStatus(source) {
    return pageAudit(source)?.status;
}

/**
 * The page with an audit stamp for its current content, replacing any audit
 * field in place or appending one to the frontmatter. The result is always
 * LF-terminated and computes to `audited`.
 *
 * @param {string} source
 * @param {{ commit: string, date: string }} at
 * @returns {string}
 */
export function stampAudit(source, { commit, date }) {
    if (!COMMIT_PATTERN.test(commit)) {
        throw new Error(`audit commit "${commit}" is not a full 40-hex sha`);
    }
    if (!isCalendarDate(date)) {
        throw new Error(
            `audit date "${date}" is not a YYYY-MM-DD calendar day`,
        );
    }
    const parts = splitFrontmatter(normalize(source));
    if (!parts) throw new Error('page has no YAML frontmatter to stamp');
    const block = [
        'audit:',
        `  commit: ${commit}`,
        `  date: ${date}`,
        `  digest: ${auditDigest(source)}`,
    ];
    const range = auditRange(parts.lines) ?? {
        start: parts.lines.length,
        end: parts.lines.length,
    };
    const lines = parts.lines.toSpliced(
        range.start,
        range.end - range.start,
        ...block,
    );
    return join(lines, parts.rest);
}
