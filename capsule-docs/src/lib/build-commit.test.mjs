import { describe, expect, it, vi } from 'vitest';
import { resolveBuildCommit } from './build-commit.mjs';

const A = 'a'.repeat(40);
const B = 'b'.repeat(40);
const C = 'c'.repeat(40);

describe('resolveBuildCommit', () => {
    it('prefers Cloudflare Pages, then GitHub Actions, then git', () => {
        const git = () => C;
        expect(
            resolveBuildCommit({ CF_PAGES_COMMIT_SHA: A, GITHUB_SHA: B }, git)
                ?.sha,
        ).toBe(A);
        expect(resolveBuildCommit({ GITHUB_SHA: B }, git)?.sha).toBe(B);
        expect(resolveBuildCommit({}, git)?.sha).toBe(C);
    });

    it('does not ask git when the environment names the commit', () => {
        const git = vi.fn(() => C);
        resolveBuildCommit({ CF_PAGES_COMMIT_SHA: A }, git);
        expect(git).not.toHaveBeenCalled();
    });

    it('shortens to seven characters and normalizes case and whitespace', () => {
        expect(
            resolveBuildCommit(
                {},
                () => '  ABCDEF0123456789abcdef0123456789ABCDEF01\n',
            ),
        ).toEqual({
            sha: 'abcdef0123456789abcdef0123456789abcdef01',
            short: 'abcdef0',
        });
    });

    it.each([
        ['empty', ''],
        ['not hex', 'not-a-commit'],
        ['too short', 'abc12'],
        ['too long', 'a'.repeat(41)],
    ])('skips an invalid environment value (%s)', (_, value) => {
        expect(
            resolveBuildCommit({ CF_PAGES_COMMIT_SHA: value }, () => C)?.sha,
        ).toBe(C);
    });

    it('returns null when no source names a commit', () => {
        expect(resolveBuildCommit({}, () => null)).toBeNull();
        expect(resolveBuildCommit({ GITHUB_SHA: 'nope' }, () => '')).toBeNull();
    });
});
