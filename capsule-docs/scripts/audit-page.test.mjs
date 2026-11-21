import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { auditStatus, parseAudit } from '../src/lib/page-audit.mjs';
import { auditPages } from './audit-page.mjs';

const COMMIT = 'ba223935c7f6424681953906c7cba931c3765bca';
const ROOT = resolve('/repo');
const PAGE = '---\ntitle: A\naudit: none\n---\n\nBody.\n';

/**
 * A fake repository: `head` holds each file's committed text, `disk` the
 * working copy, both keyed by path relative to ROOT.
 */
function fakeIo({ head = {}, disk = {} }) {
    const written = new Map();
    const logs = [];
    return {
        written,
        logs,
        io: {
            git: (args) => {
                // Raw, as git prints it: rev-parse ends in a newline and
                // show returns the committed bytes exactly.
                if (args[0] === 'rev-parse' && args[1] === 'HEAD')
                    return `${COMMIT}\n`;
                if (args[1] === '--show-toplevel') return `${ROOT}\n`;
                if (args[0] === 'show') {
                    const rel = args[1].replace(/^HEAD:/, '');
                    if (!(rel in head)) throw new Error('not in HEAD');
                    return head[rel];
                }
                throw new Error(`unexpected git ${args.join(' ')}`);
            },
            read: (path) => disk[path.slice(ROOT.length + 1)],
            write: (path, text) =>
                written.set(path.slice(ROOT.length + 1), text),
            now: () => new Date('2026-11-20T23:59:00Z'),
            log: (line) => logs.push(JSON.parse(line)),
        },
    };
}

describe('auditPages', () => {
    it('stamps a committed page with HEAD and the UTC date', () => {
        const { io, written, logs } = fakeIo({
            head: { 'docs/a.md': PAGE },
            disk: { 'docs/a.md': PAGE },
        });
        const [record] = auditPages([resolve(ROOT, 'docs/a.md')], io);
        const stamped = written.get('docs/a.md');
        expect(auditStatus(stamped)).toBe('audited');
        expect(parseAudit(stamped)).toMatchObject({
            state: 'stamp',
            commit: COMMIT,
            date: '2026-11-20',
        });
        expect(record).toMatchObject({ previous: 'none', commit: COMMIT });
        expect(logs.map((l) => l.event)).toEqual([
            'audit.start',
            'audit.stamped',
        ]);
    });

    it('restamps a page whose only change since HEAD is its audit field', () => {
        const dirtyAudit = PAGE.replace('audit: none', 'audit: none # retry');
        const { io, written } = fakeIo({
            head: { 'a.md': PAGE },
            disk: { 'a.md': dirtyAudit },
        });
        auditPages([resolve(ROOT, 'a.md')], io);
        expect(auditStatus(written.get('a.md'))).toBe('audited');
    });

    it('refuses every page, writing none, when one differs from HEAD', () => {
        const { io, written, logs } = fakeIo({
            head: { 'a.md': PAGE, 'b.md': PAGE },
            disk: { 'a.md': PAGE, 'b.md': PAGE.replace('Body', 'Edited') },
        });
        expect(() =>
            auditPages([resolve(ROOT, 'a.md'), resolve(ROOT, 'b.md')], io),
        ).toThrow(/b\.md: differs from HEAD/);
        expect(written.size).toBe(0);
        expect(logs.at(-1)).toMatchObject({ event: 'audit.refused' });
    });

    it('refuses a page whose trailing newlines differ from HEAD', () => {
        // The digest covers every byte, so the pinned commit must hold them.
        const { io, written } = fakeIo({
            head: { 'a.md': PAGE.trimEnd() },
            disk: { 'a.md': PAGE },
        });
        expect(() => auditPages([resolve(ROOT, 'a.md')], io)).toThrow(
            /differs from HEAD/,
        );
        expect(written.size).toBe(0);
    });

    it('refuses an uncommitted page', () => {
        const { io } = fakeIo({ disk: { 'new.md': PAGE } });
        expect(() => auditPages([resolve(ROOT, 'new.md')], io)).toThrow(
            /not committed/,
        );
    });

    it('refuses a generated page', () => {
        const generated = '---\ntitle: CLI\ngenerated:\n  surface: CLI\n---\n';
        const { io } = fakeIo({
            head: { 'g.md': generated },
            disk: { 'g.md': generated },
        });
        expect(() => auditPages([resolve(ROOT, 'g.md')], io)).toThrow(
            /generated page/,
        );
    });

    it('requires at least one page', () => {
        expect(() => auditPages([], fakeIo({}).io)).toThrow(/usage/);
    });
});
