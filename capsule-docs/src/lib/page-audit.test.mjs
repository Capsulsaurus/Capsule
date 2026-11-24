import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import {
    AUDIT_STATUSES,
    auditDigest,
    auditStatus,
    pageAudit,
    parseAudit,
    stampAudit,
    stripAuditBlock,
} from './page-audit.mjs';

const COMMIT = 'ba223935c7f6424681953906c7cba931c3765bca';
const sha256 = (text) =>
    `sha256:${createHash('sha256').update(text).digest('hex')}`;
const page = (audit, body = 'Body text.\n') =>
    `---\ntitle: A\n${audit}description: B\n---\n\n${body}`;
const stamp = (digest, commit = COMMIT, date = '2026-11-20') =>
    `audit:\n  commit: ${commit}\n  date: ${date}\n  digest: ${digest}\n`;

describe('AUDIT_STATUSES', () => {
    it('is the closed set of computed states', () => {
        expect(AUDIT_STATUSES).toEqual(['audited', 'outdated', 'unaudited']);
    });
});

describe('stripAuditBlock', () => {
    const bare = page('');

    it('removes a one-line audit field', () => {
        expect(stripAuditBlock(page('audit: none\n'))).toBe(bare);
    });

    it('removes a stamp and its indented lines', () => {
        expect(stripAuditBlock(page(stamp(sha256('x'))))).toBe(bare);
    });

    it('normalizes CRLF to LF', () => {
        expect(
            stripAuditBlock(page('audit: none\n').replaceAll('\n', '\r\n')),
        ).toBe(bare);
    });

    it('leaves an audit: line in the body alone', () => {
        const source = page('', 'audit: none\n');
        expect(stripAuditBlock(source)).toBe(source);
    });

    it('leaves a page without frontmatter alone', () => {
        expect(stripAuditBlock('audit: none\n')).toBe('audit: none\n');
    });
});

describe('auditDigest', () => {
    it('hashes the page without its audit block', () => {
        expect(auditDigest(page('audit: none\n'))).toBe(sha256(page('')));
    });

    it('does not depend on the audit block or line endings', () => {
        const a = auditDigest(page('audit: none\n'));
        expect(auditDigest(page(stamp(sha256('y'))))).toBe(a);
        expect(auditDigest(page('').replaceAll('\n', '\r\n'))).toBe(a);
    });

    it('changes with the body and with other frontmatter', () => {
        const a = auditDigest(page(''));
        expect(auditDigest(page('', 'Other.\n'))).not.toBe(a);
        expect(auditDigest(page('sidebar:\n  order: 2\n'))).not.toBe(a);
    });
});

describe('parseAudit', () => {
    it.each([
        ['absent', page(''), { state: 'absent' }],
        ['none', page('audit: none\n'), { state: 'none' }],
        ['quoted none', page("audit: 'none' # todo\n"), { state: 'none' }],
        ['no frontmatter', 'audit: none\n', { state: 'absent' }],
        ['a byte-order mark', `﻿${page('audit: none\n')}`, { state: 'none' }],
        [
            'fences with trailing blanks',
            '--- \ntitle: A\naudit: none\n---\t\nBody.\n',
            { state: 'none' },
        ],
    ])('%s', (_, source, expected) => {
        expect(parseAudit(source)).toEqual(expected);
    });

    it('reads a stamp', () => {
        const digest = sha256('x');
        expect(parseAudit(page(stamp(digest)))).toEqual({
            state: 'stamp',
            commit: COMMIT,
            date: '2026-11-20',
            digest,
        });
    });

    it.each([
        ['an unknown scalar', 'audit: yes\n'],
        ['a flow mapping', `audit: { commit: ${COMMIT} }\n`],
        ['a short commit', stamp(sha256('x'), 'ba223935')],
        ['an uppercase commit', stamp(sha256('x'), COMMIT.toUpperCase())],
        ['a bad date', stamp(sha256('x'), COMMIT, '2026-11-20T00:00')],
        ['a month out of range', stamp(sha256('x'), COMMIT, '2026-13-01')],
        ['a day out of range', stamp(sha256('x'), COMMIT, '2026-02-30')],
        ['a bad digest', stamp('sha1:abc')],
        [
            'a missing field',
            `audit:\n  commit: ${COMMIT}\n  date: 2026-11-20\n`,
        ],
        ['an unknown field', `${stamp(sha256('x'))}  by: someone\n`],
        ['a repeated field', `${stamp(sha256('x'))}  date: 2026-11-21\n`],
        ['an empty block', 'audit:\n'],
    ])('rejects %s', (_, audit) => {
        expect(parseAudit(page(audit)).state).toBe('malformed');
    });
});

describe('auditStatus / pageAudit', () => {
    it('is undefined for a page with no audit field', () => {
        expect(auditStatus(page(''))).toBeUndefined();
        expect(pageAudit(page(''))).toBeUndefined();
    });

    it('is unaudited for audit: none', () => {
        expect(auditStatus(page('audit: none\n'))).toBe('unaudited');
        expect(pageAudit(page('audit: none\n'))).toEqual({
            status: 'unaudited',
        });
    });

    it('is audited when the digest matches the page', () => {
        const digest = auditDigest(page(''));
        expect(pageAudit(page(stamp(digest)))).toEqual({
            status: 'audited',
            commit: COMMIT,
            date: '2026-11-20',
        });
    });

    it('is outdated once the page changes after its stamp', () => {
        const digest = auditDigest(page(''));
        const edited = page(stamp(digest), 'Edited.\n');
        expect(auditStatus(edited)).toBe('outdated');
        expect(pageAudit(edited)).toEqual({
            status: 'outdated',
            commit: COMMIT,
            date: '2026-11-20',
        });
    });

    it('throws on a malformed audit, naming the page', () => {
        expect(() => pageAudit(page('audit: yes\n'), 'x.md')).toThrow(
            /x\.md.*audit/,
        );
    });
});

describe('stampAudit', () => {
    const at = { commit: COMMIT, date: '2026-11-20' };

    it('replaces audit: none in place and computes to audited', () => {
        const source = page('audit: none\n');
        const stamped = stampAudit(source, at);
        expect(stamped).toBe(page(stamp(auditDigest(source))));
        expect(auditStatus(stamped)).toBe('audited');
    });

    it('restamps an outdated page in place', () => {
        const outdated = page(stamp(sha256('old')), 'New.\n');
        const stamped = stampAudit(outdated, { ...at, date: '2026-12-01' });
        expect(parseAudit(stamped)).toMatchObject({ date: '2026-12-01' });
        expect(auditStatus(stamped)).toBe('audited');
        expect(stripAuditBlock(stamped)).toBe(stripAuditBlock(outdated));
    });

    it('appends to frontmatter that has no audit field', () => {
        const stamped = stampAudit(page(''), at);
        expect(stamped).toBe(
            `---\ntitle: A\ndescription: B\n${stamp(auditDigest(page('')))}---\n\nBody text.\n`,
        );
    });

    it.each([
        ['a short commit', { ...at, commit: 'abc' }],
        ['a bad date', { ...at, date: '20 Nov' }],
        ['an impossible date', { ...at, date: '2026-02-30' }],
    ])('refuses %s', (_, bad) => {
        expect(() => stampAudit(page('audit: none\n'), bad)).toThrow();
    });

    it('refuses a page without frontmatter', () => {
        expect(() => stampAudit('Body.\n', at)).toThrow(/frontmatter/);
    });
});
