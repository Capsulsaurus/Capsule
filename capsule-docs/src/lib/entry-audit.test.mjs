import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';
import { commitUrl, entryAudit } from './entry-audit.mjs';
import { auditDigest } from './page-audit.mjs';

const COMMIT = 'ba223935c7f6424681953906c7cba931c3765bca';

describe('entryAudit', () => {
    it('reads nothing for an entry with no audit field', () => {
        const read = vi.fn();
        expect(
            entryAudit({ id: 'g', filePath: 'g.md', data: {} }, read),
        ).toBeUndefined();
        expect(read).not.toHaveBeenCalled();
    });

    it('reads nothing for a plugin route with no file', () => {
        const read = vi.fn();
        expect(
            entryAudit({ id: 'blog', data: { audit: 'none' } }, read),
        ).toBeUndefined();
        expect(read).not.toHaveBeenCalled();
    });

    it('computes the status from the file on disk', () => {
        const bare = '---\ntitle: A\n---\nBody.\n';
        const source = `---\ntitle: A\naudit:\n  commit: ${COMMIT}\n  date: 2026-11-20\n  digest: ${auditDigest(bare)}\n---\nBody.\n`;
        const read = vi.fn(() => source);
        expect(
            entryAudit(
                { id: 'a', filePath: 'src/a.md', data: { audit: {} } },
                read,
            ),
        ).toEqual({ status: 'audited', commit: COMMIT, date: '2026-11-20' });
        expect(read).toHaveBeenCalledWith(resolve('src/a.md'));
    });

    it('fails on a malformed audit, naming the entry', () => {
        const read = () => '---\naudit: maybe\n---\n';
        expect(() =>
            entryAudit(
                { id: 'x/y', filePath: 'y.md', data: { audit: 1 } },
                read,
            ),
        ).toThrow(/x\/y/);
    });
});

describe('commitUrl', () => {
    it('links the commit on GitHub', () => {
        expect(commitUrl(COMMIT)).toBe(
            `https://github.com/Capsulsaurus/Capsule/commit/${COMMIT}`,
        );
    });
});
