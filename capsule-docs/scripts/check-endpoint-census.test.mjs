import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
    checkEndpointCensus,
    endpointCitations,
} from './check-endpoint-census.mjs';

let root;

/** A throwaway repo with a contract and whatever docs a case needs. */
function repo(docs, paths = {}) {
    root = mkdtempSync(join(tmpdir(), 'capsule-endpoint-census-'));
    const files = {
        'capsule-server/openapi.json': JSON.stringify({
            openapi: '3.2.0',
            paths,
        }),
        ...docs,
    };
    for (const [rel, contents] of Object.entries(files)) {
        const abs = join(root, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, contents);
    }
    return root;
}

const DOC = 'capsule-docs/src/content/docs/design/x.md';

afterEach(() => {
    if (root) rmSync(root, { recursive: true, force: true });
    root = undefined;
});

describe('endpointCitations', () => {
    it('expands the compound method form into one citation per method', () => {
        expect(
            endpointCitations('`POST/HEAD/PATCH /v1/upload`').map(
                (c) => c.citation,
            ),
        ).toEqual(['POST /v1/upload', 'HEAD /v1/upload', 'PATCH /v1/upload']);
    });

    it('drops a query string and a trailing ellipsis', () => {
        expect(endpointCitations('`GET /v1/sync?cursor=…`')[0].citation).toBe(
            'GET /v1/sync',
        );
    });

    it('finds bare paths only under an API namespace', () => {
        const found = endpointCitations(
            '`/v1/quota` `/d/{opaque_id}` `/u/{opaque_id}` `/design/`',
        );
        // `/u/` is capsule-web's page namespace; `/design/` is a site route.
        expect(found.map((c) => c.citation)).toEqual([
            '/v1/quota',
            '/d/{opaque_id}',
        ]);
    });

    it('scans fenced blocks, where a wrong curl example is most harmful', () => {
        expect(
            endpointCitations('```sh\ncurl `POST /v1/gone`\n```').map(
                (c) => c.citation,
            ),
        ).toEqual(['POST /v1/gone']);
    });
});

describe('checkEndpointCensus', () => {
    it('accepts a citation the contract serves', () => {
        const { findings, checked } = checkEndpointCensus(
            repo(
                { [DOC]: '`POST /v1/upload`\n' },
                { '/v1/upload': { post: {} } },
            ),
        );
        expect(findings).toEqual([]);
        expect(checked).toBe(1);
    });

    it('matches the path template exactly, so parameter drift is a finding', () => {
        const { findings } = checkEndpointCensus(
            repo(
                { [DOC]: '`POST /v1/albums/{id}/upgrade`\n' },
                { '/v1/albums/{album_id}/upgrade': { post: {} } },
            ),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain(
            'did you mean /v1/albums/{album_id}/upgrade?',
        );
    });

    it('suggests the versioned path when a citation omits the prefix', () => {
        const { findings } = checkEndpointCensus(
            repo(
                { [DOC]: '`GET /blob/{hash}`\n' },
                { '/v1/blob/{hash}': { get: {} } },
            ),
        );
        expect(findings[0]).toContain('did you mean /v1/blob/{hash}?');
    });

    it('distinguishes a wrong method from a wrong path', () => {
        const { findings } = checkEndpointCensus(
            repo(
                { [DOC]: '`PATCH /v1/upload`\n' },
                { '/v1/upload': { post: {} } },
            ),
        );
        expect(findings[0]).toContain(
            'that path exists but not with this method',
        );
    });

    it('honours an allowlist entry keyed on file and citation', () => {
        const { findings } = checkEndpointCensus(
            repo(
                {
                    [DOC]: '`POST /v1/auth/validate`\n',
                    'capsule-docs/endpoint-census-allowlist.txt': `${DOC}\tPOST /v1/auth/validate\tdeliberately not ported\n`,
                },
                { '/v1/upload': { post: {} } },
            ),
        );
        expect(findings).toEqual([]);
    });

    it('reports an allowlist entry whose citation is gone', () => {
        const { findings } = checkEndpointCensus(
            repo(
                {
                    [DOC]: 'no citations here\n',
                    'capsule-docs/endpoint-census-allowlist.txt': `${DOC}\tPOST /v1/gone\tstale\n`,
                },
                {},
            ),
        );
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain('remove the stale exemption');
    });

    it('fails loudly when the contract is missing rather than passing vacuously', () => {
        root = mkdtempSync(join(tmpdir(), 'capsule-endpoint-census-'));
        const { findings } = checkEndpointCensus(root);
        expect(findings[0]).toContain('is missing');
    });
});
