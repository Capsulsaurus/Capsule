import {
    mkdirSync,
    mkdtempSync,
    readFileSync,
    rmSync,
    writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
    CLI_SURFACE,
    demoteHeadings,
    generate,
    readCliSurface,
    renderCliPage,
} from './gen-reference.mjs';

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
        expect(page).toMatch(/^title: Commands$/m);
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

    it('renders the usage line from the argument surface', () => {
        expect(renderCliPage(MINIMAL_CLI)).toContain(
            'capsule import <PATH>... [OPTIONS]',
        );
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
    });
});
