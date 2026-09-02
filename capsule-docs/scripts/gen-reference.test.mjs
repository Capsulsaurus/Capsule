import {
    copyFileSync,
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    rmSync,
    writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
    bucketOperations,
    CLI_SURFACE,
    demoteHeadings,
    generate,
    OPENAPI_DOCUMENT,
    readCliSurface,
    readOpenApiDocument,
    renderApiPage,
    renderCliPage,
} from './gen-reference.mjs';
import { headingAnchors } from './lib/markdown.mjs';
import { API_GROUPS, groupForPath } from './reference-groups.mjs';

/**
 * A repo-shaped temporary root: the two description artifacts at the paths the generator
 * reads, and the content directory it writes into.
 *
 * Fixtures rather than the committed artifacts wherever the assertion is about the
 * *generator*. The committed ones are used only where the assertion is about this
 * repository — that every operation it declares reaches a page.
 */
function fixtureRoot() {
    const root = mkdtempSync(join(tmpdir(), 'gen-reference-'));
    mkdirSync(join(root, 'capsule-cli'), { recursive: true });
    mkdirSync(join(root, 'capsule-server'), { recursive: true });
    mkdirSync(join(root, 'capsule-docs/src/content/docs/reference'), {
        recursive: true,
    });
    return root;
}

const MINIMAL_CLI = {
    schema: 1,
    name: 'capsule',
    about: 'A command line interface for Capsule',
    subcommands: [
        {
            name: 'import',
            about: 'Import files into a local Capsule library',
            args: [
                {
                    id: 'paths',
                    positional: true,
                    required: true,
                    repeatable: true,
                    takes_value: true,
                    value_names: ['PATH'],
                    help: 'Source file or directory to import',
                },
                {
                    id: 'provider',
                    long: 'provider',
                    positional: false,
                    required: false,
                    repeatable: false,
                    takes_value: true,
                    value_names: ['PROVIDER'],
                    possible_values: [
                        { name: 'takeout', help: 'A Takeout export' },
                    ],
                    help: 'Read the source as an export from this service',
                },
                {
                    id: 'force',
                    long: 'force',
                    short: 'f',
                    positional: false,
                    required: false,
                    repeatable: false,
                    takes_value: false,
                    help: 'Re-import files even if they already exist',
                },
            ],
        },
    ],
};

/** HTTP methods a path item may carry, per OpenAPI. */
const METHODS = new Set([
    'get',
    'put',
    'post',
    'delete',
    'options',
    'head',
    'patch',
    'trace',
]);

let root;

beforeEach(() => {
    root = fixtureRoot();
});

afterEach(() => {
    rmSync(root, { recursive: true, force: true });
});

function writeCli(surface) {
    writeFileSync(
        join(root, CLI_SURFACE),
        `${JSON.stringify(surface, null, 2)}\n`,
    );
}

function writeOpenApi(document) {
    mkdirSync(join(root, 'capsule-server'), { recursive: true });
    writeFileSync(
        join(root, OPENAPI_DOCUMENT),
        `${JSON.stringify(document, null, 2)}\n`,
    );
}

const MINIMAL_OPENAPI = {
    openapi: '3.2.0',
    info: { title: 'API', version: '0.0.0' },
    paths: {
        '/v1/version': {
            get: {
                summary: 'The protocol range this server speaks.',
                operationId: 'version',
                responses: {
                    200: {
                        description: 'The range.',
                        content: {
                            'application/json': {
                                schema: {
                                    $ref: '#/components/schemas/VersionResponse',
                                },
                            },
                        },
                    },
                },
            },
        },
        '/v1/auth/login': {
            post: {
                summary: 'Exchange an email and password for a session.',
                description:
                    'Leading prose.\n\n# Two statuses, because there are two outcomes\n\nMore prose.',
                operationId: 'login_user',
                security: [{ bearer: [] }],
                requestBody: {
                    required: true,
                    content: {
                        'application/json': {
                            schema: {
                                $ref: '#/components/schemas/LoginRequest',
                            },
                        },
                    },
                },
                responses: {
                    401: { description: 'Invalid credentials' },
                    200: {
                        description: 'A session was opened.',
                        headers: {
                            'WWW-Authenticate': { schema: { type: 'string' } },
                        },
                        content: {
                            'application/json': {
                                schema: {
                                    $ref: '#/components/schemas/TokenResponse',
                                },
                            },
                        },
                    },
                },
            },
        },
        '/v1/albums/{album_id}/ops': {
            post: {
                summary: 'Apply a lifecycle write.',
                operationId: 'album_ops',
                parameters: [
                    {
                        name: 'album_id',
                        in: 'path',
                        description: "The album's identifier.",
                        required: true,
                        schema: { type: 'string' },
                    },
                ],
                responses: { 204: { description: 'Applied.' } },
            },
        },
    },
    components: {
        securitySchemes: {
            bearer: { type: 'http', scheme: 'bearer', bearerFormat: 'JWT' },
        },
        schemas: {
            VersionResponse: {
                type: 'object',
                title: 'VersionResponse',
                required: ['min'],
                properties: {
                    min: { type: 'integer', description: 'Lowest supported.' },
                },
            },
            LoginRequest: {
                type: 'object',
                title: 'LoginRequest',
                required: ['email'],
                properties: {
                    email: {
                        type: 'string',
                        description: 'The account email.',
                    },
                    device_id: {
                        type: ['string', 'null'],
                        description: 'Advisory. Gates nothing | really.',
                    },
                },
            },
            TokenResponse: {
                type: 'object',
                title: 'TokenResponse',
                properties: {
                    access: { type: 'string' },
                    kind: { $ref: '#/components/schemas/TokenKind' },
                },
            },
            TokenKind: {
                type: 'string',
                enum: ['bearer'],
                description: 'What the token is.',
            },
        },
    },
};

describe('demoteHeadings', () => {
    // `openapi.json` really does carry `# Two statuses, because there are two outcomes`
    // inside an operation description. Rendered as-is it injects a second H1 into a page
    // whose H1 is the Starlight title, and breaks the document outline for a screen reader.
    it('demotes an ATX heading by the offset', () => {
        expect(demoteHeadings('# Why it signs you in\n', 3)).toBe(
            '#### Why it signs you in\n',
        );
        expect(demoteHeadings('## Second\n', 3)).toBe('##### Second\n');
    });

    it('clamps at h6 rather than emitting a run of seven hashes', () => {
        expect(demoteHeadings('##### Deep\n', 3)).toBe('###### Deep\n');
    });

    it('leaves prose and a hash that is not a heading alone', () => {
        expect(demoteHeadings('a #tag and #hash\n', 2)).toBe(
            'a #tag and #hash\n',
        );
        expect(demoteHeadings('body text\n', 2)).toBe('body text\n');
    });

    // A closing fence carries no info string. Treating any ``` run as a closer ends the
    // block at the nested opener, which both demotes the example's comments and leaves
    // every real heading after it untouched — two failures from one input.
    it('does not let a nested fence with an info string close the block', () => {
        const source = ['```sh', '# a', '```js', '# b', '```', '# c'].join(
            '\n',
        );
        expect(demoteHeadings(source, 2)).toBe(
            ['```sh', '# a', '```js', '# b', '```', '### c'].join('\n'),
        );
    });

    // Leaving one alone silently is worse than not supporting it: an undemoted h1 in the
    // page body is the exact defect demotion exists to prevent, and it would publish
    // looking fine.
    it('refuses a setext heading rather than silently leaving it undemoted', () => {
        expect(() => demoteHeadings('Title\n=====\n', 2)).toThrow(/setext/i);
        expect(() => demoteHeadings('Title\n-----\n', 2)).toThrow(/Title/);
    });

    // CommonMark's underline is `= +` / `- +`, not two or more. Requiring two let the
    // single-character form through the check that exists to catch it — the defect arriving
    // through the guard against the defect.
    it('refuses a single-character setext underline', () => {
        expect(() => demoteHeadings('Title\n-\n', 2)).toThrow(/setext/i);
        expect(() => demoteHeadings('Title\n=\n', 2)).toThrow(/setext/i);
    });

    it('refuses an indented or trailing-spaced single-character underline', () => {
        expect(() => demoteHeadings('Title\n   -\n', 2)).toThrow(/setext/i);
        expect(() => demoteHeadings('Title\n= \n', 2)).toThrow(/setext/i);
    });

    it('does not mistake a thematic break or a table for a setext underline', () => {
        expect(() => demoteHeadings('para\n\n---\n', 2)).not.toThrow();
        expect(() => demoteHeadings('| a |\n| --- |\n', 2)).not.toThrow();
        expect(() => demoteHeadings('- item\n---\n', 2)).not.toThrow();
    });

    it('does not mistake an underline inside a fenced example for one', () => {
        expect(() =>
            demoteHeadings('```text\nTitle\n=====\n```\n', 2),
        ).not.toThrow();
    });

    it('does not demote a hash inside a fenced block', () => {
        const source = ['```sh', '# not a heading', '```', '# heading'].join(
            '\n',
        );
        expect(demoteHeadings(source, 2)).toBe(
            ['```sh', '# not a heading', '```', '### heading'].join('\n'),
        );
    });
});

describe('readCliSurface', () => {
    it('names the missing artifact rather than emitting a stub page', () => {
        expect(() => readCliSurface(root)).toThrow(
            /capsule-cli\/cli-surface\.json/,
        );
    });

    // A stub would be the "confidently wrong" page `developer-docs.md` exists to prevent:
    // it publishes, it looks like reference, and it documents nothing.
    it('refuses a schema version it was not written against', () => {
        writeCli({ ...MINIMAL_CLI, schema: 2 });
        expect(() => readCliSurface(root)).toThrow(/schema/i);
    });

    // A well-formed but empty document used to render an empty h2, an empty usage fence, and
    // a `status: stable` badge — a page that builds green and documents nothing.
    it('refuses a document that parses but describes nothing', () => {
        writeCli({ schema: 1 });
        expect(() => readCliSurface(root)).toThrow(/no command name/);
        writeCli({ schema: 1, name: 'capsule', subcommands: [] });
        expect(() => readCliSurface(root)).toThrow(/no subcommands/);
    });

    it('refuses an unparseable artifact', () => {
        writeFileSync(join(root, CLI_SURFACE), '{ not json');
        expect(() => readCliSurface(root)).toThrow(/cli-surface\.json/);
    });
});

describe('renderCliPage', () => {
    it('renders every command in the tree', () => {
        const page = renderCliPage(MINIMAL_CLI);
        expect(page).toContain('## capsule');
        expect(page).toContain('### capsule import');
        expect(page).toContain('Import files into a local Capsule library');
    });

    it('opens with frontmatter carrying a status the schema accepts', () => {
        const page = renderCliPage(MINIMAL_CLI);
        expect(page.startsWith('---\n')).toBe(true);
        expect(page).toMatch(/^status: stable$/m);
        expect(page).toMatch(/^title: "Commands"$/m);
    });

    // A generated page is linted like any other on a machine that has built the site, and
    // a run of blank lines is the shape section assembly leaves behind.
    it('ends with exactly one newline and no run of blank lines', () => {
        const page = renderCliPage(MINIMAL_CLI);
        expect(page.endsWith('\n')).toBe(true);
        expect(page.endsWith('\n\n')).toBe(false);
        expect(page).not.toMatch(/\n{3,}/);
    });

    it('says it is generated, and by what', () => {
        expect(renderCliPage(MINIMAL_CLI)).toContain('gen-reference.mjs');
    });

    // The page body must not contain an h1: Starlight renders the frontmatter title as the
    // page's only h1, and a second one breaks the outline.
    it('emits no h1 in the body', () => {
        const body = renderCliPage(MINIMAL_CLI)
            .split('\n---\n')
            .slice(1)
            .join('\n---\n');
        expect(body.split('\n').filter((l) => /^# /.test(l))).toEqual([]);
    });

    it('spells a positional, a value-taking option, and a flag differently', () => {
        const page = renderCliPage(MINIMAL_CLI);
        expect(page).toContain('`<PATH>...`');
        expect(page).toContain('`--provider <PROVIDER>`');
        expect(page).toContain('`-f, --force`');
    });

    // A usage line that folds a required option into `[OPTIONS]` hands the reader a command
    // that fails to parse. `capsule import` really does require `--library <PATH>`.
    it('spells required options in the usage line rather than hiding them', () => {
        const surface = structuredClone(MINIMAL_CLI);
        surface.subcommands[0].args.push({
            id: 'library',
            long: 'library',
            positional: false,
            required: true,
            repeatable: false,
            takes_value: true,
            value_names: ['PATH'],
            help: 'Path to the library',
        });
        expect(renderCliPage(surface)).toContain(
            'capsule import <PATH>... --library <PATH> [OPTIONS]',
        );
    });

    it('renders the usage line from the argument surface', () => {
        expect(renderCliPage(MINIMAL_CLI)).toContain(
            'capsule import <PATH>... [OPTIONS]',
        );
    });

    // Markdown passes raw HTML through, so an unescaped placeholder disappears from the
    // rendered page — and an unclosed one takes the rest of the cell with it.
    it('escapes an angle bracket in help text', () => {
        const surface = structuredClone(MINIMAL_CLI);
        surface.subcommands[0].args[2].help = 'pass a <token> here';
        const page = renderCliPage(surface);
        expect(page).toContain('pass a &lt;token> here');
        expect(page).not.toContain('pass a <token> here');
    });

    // The defect a per-fragment escape reintroduces: the help text is escaped, the fragments
    // appended after it are not, and a `|` in a default opens a column of its own.
    it('escapes a pipe in a default value and in an enumerated value', () => {
        const surface = structuredClone(MINIMAL_CLI);
        surface.subcommands[0].args[1].possible_values = [{ name: 'x|y' }];
        surface.subcommands[0].args[1].default_values = ['a|b'];
        const page = renderCliPage(surface);
        expect(page).toContain('Values: `x\\|y`.');
        expect(page).toContain('Default: `a\\|b`.');
        expect(page).not.toContain('`x|y`');
        expect(page).not.toContain('`a|b`');
    });

    it('keeps every table row at the width of its header', () => {
        const surface = structuredClone(MINIMAL_CLI);
        surface.subcommands[0].args[1].default_values = ['a|b'];
        surface.subcommands[0].args[2].help = 'takes a | and a <tag>';
        for (const line of renderCliPage(surface).split('\n')) {
            if (!line.startsWith('|')) continue;
            const columns = line.replace(/\\\|/g, '').split('|').length;
            expect(columns).toBe(4);
        }
    });

    it('does not append "Repeatable." when the help already says it', () => {
        const surface = structuredClone(MINIMAL_CLI);
        surface.subcommands[0].args[0].help =
            'Flag an asset as a keeper (repeatable)';
        const page = renderCliPage(surface);
        expect(page).toContain('(repeatable).');
        expect(page).not.toContain('(repeatable). Repeatable.');
    });

    // The anchors are hand-built by joining command words, and the headings are slugged by
    // Starlight. Pinning them to one slugger here catches a divergence in a unit test rather
    // than in a link-validator failure at build time.
    it('emits only anchors its own headings answer', () => {
        const page = renderCliPage(MINIMAL_CLI);
        const anchors = headingAnchors(page);
        const linked = [...page.matchAll(/\]\(#([^)]+)\)/g)].map(
            (match) => match[1],
        );
        expect(linked.length).toBeGreaterThan(0);
        for (const anchor of linked) {
            expect(anchors.has(anchor)).toBe(true);
        }
    });

    it('lists an enumerated option value', () => {
        expect(renderCliPage(MINIMAL_CLI)).toContain('`takeout`');
    });

    // `clap` strips the full stop off a doc comment, so without this the appended facts run
    // straight on: "…folded into the imported assets Values: `takeout`."
    it('terminates help text before appending the facts after it', () => {
        const page = renderCliPage(MINIMAL_CLI);
        expect(page).toContain(
            'Read the source as an export from this service. Values: `takeout`.',
        );
        expect(page).toContain(
            '**Required.** Source file or directory to import. Repeatable.',
        );
    });

    it('escapes a pipe so it cannot break out of a table cell', () => {
        const page = renderCliPage({
            schema: 1,
            name: 'capsule',
            subcommands: [
                {
                    name: 'x',
                    args: [
                        {
                            id: 'p',
                            long: 'p',
                            positional: false,
                            required: false,
                            repeatable: false,
                            takes_value: false,
                            help: 'reads a | b',
                        },
                    ],
                },
            ],
        });
        expect(page).toContain('reads a \\| b');
    });
});

describe('generate', () => {
    it('is deterministic: two runs produce byte-identical pages', () => {
        writeCli(MINIMAL_CLI);
        writeOpenApi(MINIMAL_OPENAPI);
        const first = generate(root).map((path) =>
            readFileSync(join(root, path), 'utf8'),
        );
        const second = generate(root).map((path) =>
            readFileSync(join(root, path), 'utf8'),
        );
        expect(second).toEqual(first);
        expect(first.length).toBeGreaterThan(0);
    });

    it('fails, writing nothing, when an artifact is missing', () => {
        expect(() => generate(root)).toThrow(/cli-surface\.json/);
        expect(
            existsSync(
                join(
                    root,
                    'capsule-docs/src/content/docs/reference/cli/commands.md',
                ),
            ),
        ).toBe(false);
    });

    // A page a later run no longer emits stays on disk otherwise, and the directory is
    // gitignored, so nothing ever shows it: Astro keeps routing and indexing a page no
    // artifact describes, on this machine and on no CI runner.
    it('clears output it no longer emits', () => {
        writeCli(MINIMAL_CLI);
        writeOpenApi(MINIMAL_OPENAPI);
        generate(root);
        const orphan = join(
            root,
            'capsule-docs/src/content/docs/reference/api/retired-group.md',
        );
        writeFileSync(orphan, '---\ntitle: Gone\nstatus: stable\n---\n');
        generate(root);
        expect(existsSync(orphan)).toBe(false);
    });

    // The overviews are siblings of the generated directories precisely so that clearing
    // one cannot take a hand-written page with it.
    it('does not clear the hand-written overview beside the generated directory', () => {
        writeCli(MINIMAL_CLI);
        writeOpenApi(MINIMAL_OPENAPI);
        const overview = join(
            root,
            'capsule-docs/src/content/docs/reference/cli.md',
        );
        writeFileSync(overview, '---\ntitle: CLI\nstatus: draft\n---\n');
        generate(root);
        expect(existsSync(overview)).toBe(true);
    });
});

describe('link rewriting in artifact prose', () => {
    // Both forms are live in the committed OpenAPI document. Rendered unchanged they are
    // three broken links that fail `starlight-links-validator` and the docs build with it.
    it('rewrites a repo-relative design-doc path to its site route', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/auth/login'].post.description =
            'Every rule the [chunk contract](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md) fixes.';
        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'auth'),
            bucketOperations(document).get('auth'),
            document,
        );
        expect(rendered).toContain(
            '[chunk contract](/design/import/upload-protocol/)',
        );
    });

    it('drops a rustdoc intra-doc link and keeps its text', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/auth/login'].post.description =
            'A signature over [`revoke_all_signing_bytes`](capsule_core::crypto::revoke::revoke_all_signing_bytes), in CBOR.';
        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'auth'),
            bucketOperations(document).get('auth'),
            document,
        );
        expect(rendered).toContain(
            'A signature over `revoke_all_signing_bytes`, in CBOR.',
        );
        expect(rendered).not.toContain('capsule_core::crypto::revoke');
    });

    // A model's own doc comment is as free to cite a design document by repo path, or an
    // item by its rustdoc path, as a handler's is — and the schema appendix is a separate
    // render path that had to be wired up for it.
    it('rewrites links in a schema-level description too', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.components.schemas.LoginRequest.description =
            'Shaped by the [chunk contract](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md) and signed with [`revoke_all_signing_bytes`](capsule_core::crypto::revoke::revoke_all_signing_bytes).';
        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'auth'),
            bucketOperations(document).get('auth'),
            document,
        );
        expect(rendered).toContain(
            '[chunk contract](/design/import/upload-protocol/)',
        );
        expect(rendered).toContain('signed with `revoke_all_signing_bytes`.');
        expect(rendered).not.toContain('capsule_core::crypto::revoke');
    });

    it('leaves an absolute URL, a site route, and an anchor alone', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/auth/login'].post.description =
            'See [a](https://example.invalid/x), [b](/design/i18n/), and [c](#later).';
        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'auth'),
            bucketOperations(document).get('auth'),
            document,
        );
        expect(rendered).toContain('[a](https://example.invalid/x)');
        expect(rendered).toContain('[b](/design/i18n/)');
        expect(rendered).toContain('[c](#later)');
    });
});

describe('groupForPath', () => {
    it('matches the longest prefix, not the first declared', () => {
        expect(groupForPath('/v1/auth/login')?.slug).toBe('auth');
        expect(groupForPath('/v1/albums/{album_id}/ops')?.slug).toBe('albums');
        expect(groupForPath('/s/{opaque_id}/blob/{hash}')?.slug).toBe('shares');
        expect(groupForPath('/d/{opaque_id}')?.slug).toBe('drops');
    });

    it('returns null for a path no group claims', () => {
        expect(groupForPath('/v1/search')).toBe(null);
    });
});

describe('bucketOperations', () => {
    // The gate that keeps the hand-curated navigation honest: a new endpoint family cannot
    // publish unlisted, and cannot silently not publish at all.
    it('fails, naming the operation, when an endpoint matches no group', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/search'] = {
            get: { summary: 'Search', operationId: 'search', responses: {} },
        };
        expect(() => bucketOperations(document)).toThrow(/GET \/v1\/search/);
        expect(() => bucketOperations(document)).toThrow(
            /reference-groups\.mjs/,
        );
    });

    it('buckets every operation exactly once', () => {
        const buckets = bucketOperations(MINIMAL_OPENAPI);
        const total = [...buckets.values()].reduce(
            (sum, operations) => sum + operations.length,
            0,
        );
        expect(total).toBe(3);
        expect(buckets.get('auth')?.[0].operationId).toBe('login_user');
    });

    it('orders operations within a group by path, then by method', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/auth/aaa'] = {
            post: { summary: 'a', operationId: 'a', responses: {} },
            get: { summary: 'b', operationId: 'b', responses: {} },
        };
        const auth = bucketOperations(document).get('auth');
        expect(
            auth.map((operation) => `${operation.method} ${operation.path}`),
        ).toEqual([
            'GET /v1/auth/aaa',
            'POST /v1/auth/aaa',
            'POST /v1/auth/login',
        ]);
    });
});

describe('readOpenApiDocument', () => {
    it('names the missing artifact rather than emitting a stub page', () => {
        expect(() => readOpenApiDocument(root)).toThrow(
            /capsule-server\/openapi\.json/,
        );
    });

    it('refuses a document that is not OpenAPI 3.2', () => {
        writeOpenApi({ ...MINIMAL_OPENAPI, openapi: '3.1.0' });
        expect(() => readOpenApiDocument(root)).toThrow(/3\.2/);
    });

    // This renderer shows one body per carrier. Picking the first of several silently would
    // document an endpoint that also accepts CBOR as accepting only JSON.
    it('refuses a carrier offering more than one media type, naming the operation', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/auth/login'].post.requestBody.content[
            'application/cbor'
        ] = { schema: { $ref: '#/components/schemas/LoginRequest' } };
        writeOpenApi(document);
        expect(() => readOpenApiDocument(root)).toThrow(
            /POST \/v1\/auth\/login/,
        );
        expect(() => readOpenApiDocument(root)).toThrow(/media types/);
    });

    it('refuses a multi-media-type response too', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.paths['/v1/version'].get.responses[200].content[
            'application/cbor'
        ] = { schema: { $ref: '#/components/schemas/VersionResponse' } };
        writeOpenApi(document);
        expect(() => readOpenApiDocument(root)).toThrow(/GET \/v1\/version/);
        expect(() => readOpenApiDocument(root)).toThrow(/response 200/);
    });

    // A property table cannot express a union or an intersection, so a composed schema
    // would render as an empty or a half-true model.
    it.each([
        'oneOf',
        'allOf',
        'anyOf',
    ])('refuses a schema composed with %s, naming the schema', (keyword) => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.components.schemas.TokenResponse = {
            title: 'TokenResponse',
            [keyword]: [
                { $ref: '#/components/schemas/VersionResponse' },
                { type: 'object' },
            ],
        };
        writeOpenApi(document);
        expect(() => readOpenApiDocument(root)).toThrow(/TokenResponse/);
        expect(() => readOpenApiDocument(root)).toThrow(new RegExp(keyword));
    });

    // Checking only the root is the shape of the bug it prevents: a composition one level
    // down falls through `typeOf` to `object` and renders as a row claiming the field is a
    // plain object — a confident lie about a union.
    it.each([
        'properties.choice',
        'items',
        'additionalProperties',
    ])('refuses a composition nested at %s, naming the schema and the path', (where) => {
        const document = structuredClone(MINIMAL_OPENAPI);
        const composed = {
            oneOf: [{ type: 'string' }, { type: 'integer' }],
        };
        const target = { type: 'object', title: 'TokenResponse' };
        if (where === 'properties.choice') {
            target.properties = { choice: composed };
        } else if (where === 'items') {
            target.items = composed;
        } else {
            target.additionalProperties = composed;
        }
        document.components.schemas.TokenResponse = target;
        writeOpenApi(document);
        expect(() => readOpenApiDocument(root)).toThrow(/TokenResponse/);
        expect(() => readOpenApiDocument(root)).toThrow(/oneOf/);
    });

    // A `$ref` is scanned on the target's own pass. Following it would report the same
    // composition once per reference, under whichever name was scanned first.
    it('reports a composed schema once, under its own name', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        document.components.schemas.TokenKind = {
            title: 'TokenKind',
            oneOf: [{ type: 'string' }, { type: 'integer' }],
        };
        writeOpenApi(document);
        expect(() => readOpenApiDocument(root)).toThrow(/schema TokenKind/);
    });

    it('accepts the committed document', () => {
        expect(() =>
            readOpenApiDocument(
                resolve(dirname(fileURLToPath(import.meta.url)), '..', '..'),
            ),
        ).not.toThrow();
    });
});

describe('renderApiPage', () => {
    const group = API_GROUPS.find((entry) => entry.slug === 'auth');

    function page() {
        return renderApiPage(
            group,
            bucketOperations(MINIMAL_OPENAPI).get('auth'),
            MINIMAL_OPENAPI,
        );
    }

    it('renders the method and path as the operation heading', () => {
        expect(page()).toContain('## POST /v1/auth/login');
    });

    // `openapi.json` really does carry `# Two statuses, because there are two outcomes`.
    // Interpolated unchanged it is a second h1 on the page.
    it('demotes a heading inside an operation description', () => {
        const rendered = page();
        expect(rendered).toContain(
            '#### Two statuses, because there are two outcomes',
        );
        const body = rendered.split('\n---\n').slice(1).join('\n---\n');
        expect(body.split('\n').filter((line) => /^# /.test(line))).toEqual([]);
    });

    it('says which operations require authentication', () => {
        expect(page()).toMatch(/[Bb]earer/);
    });

    it('renders the request body schema and its fields', () => {
        const rendered = page();
        expect(rendered).toContain('LoginRequest');
        expect(rendered).toContain('`email`');
        expect(rendered).toContain('The account email.');
    });

    it('escapes a pipe inside a schema description', () => {
        expect(page()).toContain('Gates nothing \\| really.');
    });

    // Every use of a type is a table cell, so an unescaped separator opens a fourth column
    // and shifts the row — `device_id` in the committed document is exactly this shape.
    it('renders a nullable union as an escaped type, not as [object Object]', () => {
        const rendered = page();
        expect(rendered).not.toContain('[object Object]');
        expect(rendered).toContain('`string \\| null`');
        expect(rendered).not.toContain('`string | null`');
    });

    it('keeps every table row at the width of its header', () => {
        for (const line of page().split('\n')) {
            if (!line.startsWith('|')) continue;
            // A cell may legitimately contain an escaped pipe; an unescaped one is a column.
            const columns = line.replace(/\\\|/g, '').split('|').length;
            expect(columns).toBeLessThanOrEqual(6);
        }
    });

    it('resolves a $ref one level and links deeper refs to their anchor', () => {
        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'version'),
            bucketOperations(MINIMAL_OPENAPI).get('version'),
            MINIMAL_OPENAPI,
        );
        expect(rendered).toContain('VersionResponse');
        expect(rendered).toContain('Lowest supported.');
    });

    it('lists responses in ascending status order', () => {
        const rendered = page();
        expect(rendered.indexOf('| `200`')).toBeLessThan(
            rendered.indexOf('| `401`'),
        );
    });

    it('renders a path parameter', () => {
        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'albums'),
            bucketOperations(MINIMAL_OPENAPI).get('albums'),
            MINIMAL_OPENAPI,
        );
        expect(rendered).toContain('`album_id`');
        expect(rendered).toContain("The album's identifier.");
    });

    it('opens with frontmatter the content schema accepts', () => {
        const rendered = page();
        expect(rendered.startsWith('---\n')).toBe(true);
        expect(rendered).toMatch(/^status: stable$/m);
        expect(rendered).toMatch(/^title: "[^"]+"$/m);
    });

    it('emits only anchors its own headings answer', () => {
        const rendered = page();
        const anchors = headingAnchors(rendered);
        const linked = [...rendered.matchAll(/\]\(#([^)]+)\)/g)].map(
            (match) => match[1],
        );
        expect(linked.length).toBeGreaterThan(0);
        for (const anchor of linked) {
            expect(anchors.has(anchor)).toBe(true);
        }
    });

    it('ends with exactly one newline and no run of blank lines', () => {
        const rendered = page();
        expect(rendered.endsWith('\n')).toBe(true);
        expect(rendered.endsWith('\n\n')).toBe(false);
        expect(rendered).not.toMatch(/\n{3,}/);
    });
});

describe('the schema appendix', () => {
    // The depth bound and the cycle guard interact. Keyed on "seen at all", the answer
    // depends on traversal order: reach a model at depth 2 first and its children are cut
    // by the bound, and the later path that reaches it at depth 1 is then refused as
    // already-seen. Keyed on the shallowest depth, both paths get their chance.
    it('expands a model when a shallower path reaches it after a deeper one', () => {
        const document = structuredClone(MINIMAL_OPENAPI);
        const schemas = document.components.schemas;
        schemas.Leaf = {
            type: 'object',
            title: 'Leaf',
            properties: {
                mark: { type: 'string', description: 'The leaf mark.' },
            },
        };
        // A chain long enough that the deep path runs past MAX_SCHEMA_DEPTH exactly at
        // `Tail`, so `Leaf` is out of reach along it.
        schemas.Tail = {
            type: 'object',
            title: 'Tail',
            properties: { leaf: { $ref: '#/components/schemas/Leaf' } },
        };
        for (const [name, next] of [
            ['Link3', 'Tail'],
            ['Link2', 'Link3'],
            ['Link1', 'Link2'],
        ]) {
            schemas[name] = {
                type: 'object',
                title: name,
                properties: { next: { $ref: `#/components/schemas/${next}` } },
            };
        }
        // Walked first (`requestBody` before `responses`): reaches `Tail` too deep to
        // expand it. The response then reaches the same name at depth 0.
        schemas.LoginRequest.properties.deep = {
            $ref: '#/components/schemas/Link1',
        };
        document.paths['/v1/auth/login'].post.responses[200].content[
            'application/json'
        ].schema = { $ref: '#/components/schemas/Tail' };

        const rendered = renderApiPage(
            API_GROUPS.find((entry) => entry.slug === 'auth'),
            bucketOperations(document).get('auth'),
            document,
        );
        expect(rendered).toContain('### Tail');
        expect(rendered).toContain('### Leaf');
        expect(rendered).toContain('The leaf mark.');
    });

    // The concrete instance of that bug in the committed document: `WireBlobRole` is
    // reachable from `/reference/api/sync/` and was named on the page while defined nowhere.
    it('documents every model the committed sync page links to', () => {
        const repoRoot = resolve(
            dirname(fileURLToPath(import.meta.url)),
            '..',
            '..',
        );
        const document = readOpenApiDocument(repoRoot);
        const group = API_GROUPS.find((entry) => entry.slug === 'sync');
        const rendered = renderApiPage(
            group,
            bucketOperations(document).get('sync'),
            document,
        );
        expect(rendered).toContain('### WireBlobRole');
        const headings = new Set(
            [...rendered.matchAll(/^#{2,6} (.+)$/gm)].map((match) =>
                match[1]
                    .toLowerCase()
                    .replace(/[^a-z0-9 -]/g, '')
                    .replace(/ /g, '-'),
            ),
        );
        for (const [, anchor] of rendered.matchAll(/\]\(#([^)]+)\)/g)) {
            expect(headings.has(anchor)).toBe(true);
        }
    });
});

describe('the committed artifacts', () => {
    // The assertion about *this repository* rather than about the generator: every
    // operation the server declares reaches a page. A group table that quietly stopped
    // covering a family would fail here as well as in `bucketOperations`.
    const repoRoot = resolve(
        dirname(fileURLToPath(import.meta.url)),
        '..',
        '..',
    );

    it('bucket every declared operation into a group', () => {
        const document = readOpenApiDocument(repoRoot);
        const declared = Object.entries(document.paths).flatMap(([, item]) =>
            Object.keys(item).filter((key) => METHODS.has(key)),
        );
        const buckets = bucketOperations(document);
        const bucketed = [...buckets.values()].reduce(
            (sum, operations) => sum + operations.length,
            0,
        );
        expect(bucketed).toBe(declared.length);
        expect(bucketed).toBeGreaterThan(50);
    });

    // Renders into a temp root seeded from the two committed artifacts, never into the
    // working tree. `check-docs` runs `test-docs` and `build-docs` in parallel, and
    // `generate` clears its output directories before writing: generating into the real
    // tree races the Astro build reading it, which fails intermittently and only under the
    // gate. The artifacts are the committed ones, so the assertion is still about this
    // repository.
    it('generate one page per group plus the CLI page', () => {
        copyFileSync(join(repoRoot, CLI_SURFACE), join(root, CLI_SURFACE));
        copyFileSync(
            join(repoRoot, OPENAPI_DOCUMENT),
            join(root, OPENAPI_DOCUMENT),
        );

        const written = generate(root);

        expect(written).toContain(
            'capsule-docs/src/content/docs/reference/cli/commands.md',
        );
        for (const group of API_GROUPS) {
            expect(written).toContain(
                `capsule-docs/src/content/docs/reference/api/${group.slug}.md`,
            );
        }
        // Every page the run reported is a page it actually wrote, under the temp root.
        for (const path of written) {
            expect(existsSync(join(root, path))).toBe(true);
        }
        expect(written).toHaveLength(API_GROUPS.length + 1);
    });
});

describe('tidyBlankLines, through the pages that use it', () => {
    // A blank run inside a fenced example is part of the example. A whole-document collapse
    // has the generator quietly editing the code it is quoting.
    it('keeps a blank run inside a fenced example', () => {
        const surface = structuredClone(MINIMAL_CLI);
        surface.subcommands[0].long_about =
            'Example:\n\n```sh\ncapsule import a\n\n\ncapsule import b\n```';
        expect(renderCliPage(surface)).toContain(
            'capsule import a\n\n\ncapsule import b',
        );
    });
});
