import { describe, expect, it } from 'vitest';
import { skipReason, staleArtifacts } from './staleness.mjs';

const CLI = 'capsule-cli/cli-surface.json';
const API = 'capsule-server/openapi.json';
const SOURCES = {
    [CLI]: { sources: ['capsule-cli/src/'], regenerate: 'x' },
    [API]: { sources: ['capsule-server/src/'], regenerate: 'y' },
};

describe('staleArtifacts', () => {
    it('is empty on a branch that touched nothing', () => {
        expect(staleArtifacts({ dirty: [], commits: [] }, SOURCES)).toEqual([]);
    });

    it('flags an uncommitted source change the artifact did not follow', () => {
        const changes = { dirty: ['capsule-cli/src/cli/mod.rs'], commits: [] };
        expect(staleArtifacts(changes, SOURCES)).toEqual([CLI]);
    });

    it('treats an uncommitted artifact as freshly regenerated', () => {
        const changes = {
            dirty: ['capsule-cli/src/cli/mod.rs', CLI],
            commits: [],
        };
        expect(staleArtifacts(changes, SOURCES)).toEqual([]);
    });

    it('flags a source commit newer than the last artifact commit', () => {
        // Newest first: the source changed after the artifact was regenerated.
        const changes = {
            dirty: [],
            commits: [['capsule-server/src/routes/x.rs'], [API]],
        };
        expect(staleArtifacts(changes, SOURCES)).toEqual([API]);
    });

    it('clears once the artifact is regenerated after the source change', () => {
        const changes = {
            dirty: [],
            commits: [[API], ['capsule-server/src/routes/x.rs']],
        };
        expect(staleArtifacts(changes, SOURCES)).toEqual([]);
    });

    it('accepts one commit that changes both', () => {
        const changes = {
            dirty: [],
            commits: [['capsule-server/src/routes/x.rs', API]],
        };
        expect(staleArtifacts(changes, SOURCES)).toEqual([]);
    });

    it('ignores paths outside every source', () => {
        const changes = {
            dirty: ['capsule-docs/README.md'],
            commits: [['capsule-server/tests/x.rs']],
        };
        expect(staleArtifacts(changes, SOURCES)).toEqual([]);
    });
});

describe('staleArtifacts, with modification times', () => {
    const changes = {
        dirty: [],
        commits: [['capsule-cli/src/cli/mod.rs'], [CLI]],
    };

    it('stays stale while a changed source is newer than the artifact', () => {
        const mtime = (path) => (path === CLI ? 1 : 2);
        expect(staleArtifacts(changes, SOURCES, mtime)).toEqual([CLI]);
    });

    it('clears once the artifact is rewritten, even with identical bytes', () => {
        const mtime = (path) => (path === CLI ? 3 : 2);
        expect(staleArtifacts(changes, SOURCES, mtime)).toEqual([]);
    });

    it('flags a source edited after an uncommitted regeneration', () => {
        const dirty = {
            dirty: [CLI, 'capsule-cli/src/cli/mod.rs'],
            commits: [],
        };
        expect(
            staleArtifacts(dirty, SOURCES, (path) => (path === CLI ? 1 : 2)),
        ).toEqual([CLI]);
        expect(
            staleArtifacts(dirty, SOURCES, (path) => (path === CLI ? 3 : 2)),
        ).toEqual([]);
    });

    it('ignores a deleted source', () => {
        const mtime = (path) => (path === CLI ? 1 : undefined);
        expect(staleArtifacts(changes, SOURCES, mtime)).toEqual([]);
    });
});

describe('skipReason', () => {
    it('skips in CI and on Cloudflare, and runs locally', () => {
        expect(skipReason({ CI: 'true' })).toMatch(/CI/);
        expect(skipReason({ CF_PAGES: '1' })).toMatch(/Cloudflare/);
        expect(skipReason({})).toBeNull();
    });
});
