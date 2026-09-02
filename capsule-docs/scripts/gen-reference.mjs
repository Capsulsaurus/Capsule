#!/usr/bin/env node

/**
 * gen-reference — emit `/reference/` from the committed description artifacts.
 *
 * `design/developer-docs.md` fixes the boundary at *artifacts, not toolchains*: the CI
 * `docs` job installs bun and nothing else, so this script may never shell out to cargo. It
 * reads committed JSON and writes Markdown, which is the whole of its contract.
 *
 * The pages it writes are ordinary content-collection entries under
 * `src/content/docs/reference/`, which is what buys the rest for free — Pagefind indexes
 * them, `starlight-links-validator` checks their links, the `PageTitle` override renders
 * their status badge, and the notranslate rehype pass marks up their technical terms. A
 * mounted OpenAPI application would have had none of that.
 *
 * They are also **gitignored**, and that is the point of rule 2: a generated page is never
 * edited, so committing one only creates a copy that can disagree with its source. Fix the
 * clap `about` or the schema description and regenerate.
 *
 * Three failure modes are deliberately fatal rather than degraded, because
 * `developer-docs.md` calls a stale reference page worse than a missing one — a missing page
 * is obvious and a wrong one is believed:
 *
 *   1. an artifact is absent or unparseable — exit naming the path;
 *   2. an artifact's `schema` is one this script was not written against — exit rather than
 *      render a half-understood document;
 *   3. an operation matches no group in `reference-groups.mjs` — exit naming it, so a new
 *      endpoint family cannot publish unlisted.
 *
 * Usage: `bun capsule-docs/scripts/gen-reference.mjs` from anywhere; `package.json` runs it
 * before `astro dev` and `astro build`.
 */

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CLI_PAGES } from './reference-groups.mjs';

/** Repo-relative path of the committed command-tree artifact. */
export const CLI_SURFACE = 'capsule-cli/cli-surface.json';

/** Command-tree schema version this script understands. */
const CLI_SCHEMA = 1;

/** Repo-relative directory the generated CLI pages are written to. */
const CLI_OUT = 'capsule-docs/src/content/docs/reference/cli';

/** The banner every generated page carries, as an HTML comment and as prose. */
const GENERATED_BY = 'capsule-docs/scripts/gen-reference.mjs';

/**
 * Shift every ATX heading in `markdown` down by `offset` levels, clamped at h6.
 *
 * Description prose in both artifacts is written for its own context and carries its own
 * headings — `openapi.json` has operation descriptions opening at `#`. Interpolated
 * unchanged, such a heading becomes a second h1 on a page whose h1 is the Starlight title,
 * which breaks the document outline and the on-page table of contents.
 *
 * Fenced blocks are skipped: a `#` on the first column of a shell example is a comment, not
 * a heading, and demoting it would corrupt the example.
 *
 * @param {string} markdown Prose that may contain headings.
 * @param {number} offset Levels to add.
 * @returns {string} The prose with its headings demoted.
 */
export function demoteHeadings(markdown, offset) {
    let fence = null;
    return markdown
        .split('\n')
        .map((line) => {
            const fenceMatch = /^\s*(`{3,}|~{3,})/.exec(line);
            if (fence === null) {
                if (fenceMatch) {
                    fence = {
                        char: fenceMatch[1][0],
                        length: fenceMatch[1].length,
                    };
                    return line;
                }
            } else {
                if (
                    fenceMatch &&
                    fenceMatch[1][0] === fence.char &&
                    fenceMatch[1].length >= fence.length
                ) {
                    fence = null;
                }
                return line;
            }
            const heading = /^(#{1,6})(\s)/.exec(line);
            if (!heading) return line;
            const level = Math.min(6, heading[1].length + offset);
            return '#'.repeat(level) + line.slice(heading[1].length);
        })
        .join('\n');
}

/**
 * Escape a string for a Markdown table cell: a literal `|` would otherwise open a new
 * column, and a newline would end the row.
 *
 * @param {string} text
 * @returns {string}
 */
function cell(text) {
    return text
        .replace(/\s*\n\s*/g, ' ')
        .replace(/\|/g, '\\|')
        .trim();
}

/**
 * Escape a YAML double-quoted scalar, for frontmatter values that carry arbitrary prose.
 *
 * @param {string} text
 * @returns {string}
 */
function yamlString(text) {
    return `"${text
        .replace(/\\/g, '\\\\')
        .replace(/"/g, '\\"')
        .replace(/\s*\n\s*/g, ' ')
        .trim()}"`;
}

/**
 * Read and validate a JSON artifact.
 *
 * @param {string} root Repository root.
 * @param {string} relPath Repo-relative artifact path.
 * @returns {unknown}
 */
function readArtifact(root, relPath) {
    let raw;
    try {
        raw = readFileSync(join(root, relPath), 'utf8');
    } catch (cause) {
        throw new Error(
            `cannot read the description artifact ${relPath}: ${cause.message}. ` +
                'Run its emitter (`mise run cli-surface`, `mise run openapi-kynos`) and commit the result.',
            { cause },
        );
    }
    try {
        return JSON.parse(raw);
    } catch (cause) {
        throw new Error(`${relPath} is not valid JSON: ${cause.message}`, {
            cause,
        });
    }
}

/**
 * The committed `capsule` command tree.
 *
 * @param {string} root Repository root.
 * @returns {Record<string, any>}
 */
export function readCliSurface(root) {
    const surface = readArtifact(root, CLI_SURFACE);
    if (surface?.schema !== CLI_SCHEMA) {
        throw new Error(
            `${CLI_SURFACE} declares schema ${surface?.schema}, and this generator was ` +
                `written against schema ${CLI_SCHEMA}. Update ${GENERATED_BY} rather than ` +
                'rendering a document it does not understand.',
        );
    }
    return surface;
}

/**
 * How an argument is spelled on the command line.
 *
 * A positional is written as its placeholder, an option by its flags. Only a value-taking
 * option gets a placeholder — the description artifact suppresses clap's synthesized one
 * for flags, and inventing `--force <FORCE>` here would document a surface that rejects it.
 *
 * @param {Record<string, any>} arg
 * @returns {string}
 */
function spell(arg) {
    const placeholder = `<${(arg.value_names ?? [arg.id.toUpperCase()]).join('> <')}>`;
    if (arg.positional) {
        return `${placeholder}${arg.repeatable ? '...' : ''}`;
    }
    const flags = [];
    if (arg.short) flags.push(`-${arg.short}`);
    if (arg.long) flags.push(`--${arg.long}`);
    const spelled = flags.length > 0 ? flags.join(', ') : arg.id;
    return arg.takes_value ? `${spelled} ${placeholder}` : spelled;
}

/**
 * Terminate a sentence that does not terminate itself.
 *
 * `clap` strips the full stop off a doc comment, so help text arrives unpunctuated and the
 * facts appended after it ("Repeatable.", "Values: …") would run straight on from the last
 * word of the description.
 *
 * @param {string} text
 * @returns {string}
 */
function sentence(text) {
    return /[.!?:]$/.test(text) ? text : `${text}.`;
}

/**
 * One argument's description cell: whether it is required and repeatable, what it does,
 * what it accepts, and what it defaults to.
 *
 * @param {Record<string, any>} arg
 * @returns {string}
 */
function describeArg(arg) {
    const parts = [];
    if (arg.required) parts.push('**Required.**');
    const help = arg.long_help ?? arg.help;
    if (help) parts.push(sentence(cell(help)));
    if (arg.repeatable) parts.push('Repeatable.');
    if (arg.possible_values?.length) {
        parts.push(
            `Values: ${arg.possible_values.map((value) => `\`${value.name}\``).join(', ')}.`,
        );
    }
    if (arg.default_values?.length) {
        parts.push(
            `Default: ${arg.default_values.map((value) => `\`${value}\``).join(', ')}.`,
        );
    }
    return parts.join(' ') || '—';
}

/**
 * A Markdown table, or the empty string when there are no rows — an empty table renders as
 * a stray header and says nothing.
 *
 * @param {string[]} headers
 * @param {string[][]} rows
 * @returns {string}
 */
function table(headers, rows) {
    if (rows.length === 0) return '';
    const lines = [
        `| ${headers.join(' | ')} |`,
        `| ${headers.map(() => '---').join(' | ')} |`,
        ...rows.map((row) => `| ${row.join(' | ')} |`),
    ];
    return `${lines.join('\n')}\n`;
}

/**
 * Render one command and, recursively, its subcommands.
 *
 * @param {Record<string, any>} command
 * @param {string[]} path Command words leading here, including this command's own name.
 * @param {number} level Heading level for this command (2 under the Starlight title).
 * @returns {string}
 */
function renderCommand(command, path, level) {
    const invocation = path.join(' ');
    const args = command.args ?? [];
    const positionals = args.filter((arg) => arg.positional);
    const options = args.filter((arg) => !arg.positional);
    const subcommands = command.subcommands ?? [];

    const usage = [invocation];
    for (const arg of positionals) {
        const spelled = spell(arg);
        usage.push(arg.required ? spelled : `[${spelled}]`);
    }
    if (options.length > 0) usage.push('[OPTIONS]');
    if (subcommands.length > 0) usage.push('<COMMAND>');

    const sections = [
        `${'#'.repeat(level)} ${invocation}`,
        '',
        `\`\`\`text\n${usage.join(' ')}\n\`\`\``,
        '',
    ];

    // Demoted relative to this command's own heading, so a doc comment that opens at `#`
    // nests under the command it describes instead of outranking it.
    const prose = command.long_about ?? command.about;
    if (prose) {
        sections.push(demoteHeadings(prose, level), '');
    }

    const positionalTable = table(
        ['Argument', 'Description'],
        positionals.map((arg) => [`\`${spell(arg)}\``, describeArg(arg)]),
    );
    if (positionalTable) sections.push(positionalTable);

    const optionTable = table(
        ['Option', 'Description'],
        options.map((arg) => [`\`${spell(arg)}\``, describeArg(arg)]),
    );
    if (optionTable) sections.push(optionTable);

    if (subcommands.length > 0) {
        sections.push(
            table(
                ['Command', 'Description'],
                subcommands.map((subcommand) => [
                    `[\`${[...path, subcommand.name].join(' ')}\`](#${[...path, subcommand.name].join('-')})`,
                    subcommand.about ? cell(subcommand.about) : '—',
                ]),
            ),
        );
    }

    return [
        sections.filter((section) => section !== '').join('\n\n'),
        ...subcommands.map((subcommand) =>
            renderCommand(
                subcommand,
                [...path, subcommand.name],
                Math.min(6, level + 1),
            ),
        ),
    ].join('\n\n');
}

/**
 * The generated `/reference/cli/commands/` page.
 *
 * @param {Record<string, any>} surface The parsed command tree.
 * @returns {string} Markdown, frontmatter included.
 */
export function renderCliPage(surface) {
    const page = CLI_PAGES[0];
    return `${[
        '---',
        `title: ${page.label}`,
        `description: ${yamlString(page.description)}`,
        'status: stable',
        '---',
        '',
        `<!-- Generated from ${CLI_SURFACE} by ${GENERATED_BY}. Do not edit. -->`,
        '',
        `Generated from \`${CLI_SURFACE}\`, the committed command tree \`capsule-cli\` emits and`,
        '`mise run cli-surface-check` keeps current. To change a description on this page, change',
        'the `clap` annotation it comes from and regenerate — this file is build output.',
        '',
        renderCommand(surface, [surface.name], 2),
    ]
        .join('\n')
        // Section assembly can leave a run of blank lines where a table ended one block and
        // a heading opened the next, and a trailing one at the end of the file. Markdown
        // does not care; a reader diffing two generations does, and so does markdownlint
        // on any machine that has built the site.
        .replace(/\n{3,}/g, '\n\n')
        .trimEnd()}\n`;
}

/**
 * Emit every generated reference page under `root`.
 *
 * Artifacts are read and validated first, before anything is written, so a missing or
 * unparseable one leaves no half-written section behind.
 *
 * @param {string} root Repository root.
 * @returns {string[]} Repo-relative paths written, in a stable order.
 */
export function generate(root) {
    const surface = readCliSurface(root);

    /** @type {Array<{ path: string, body: string }>} */
    const pages = [
        {
            path: `${CLI_OUT}/${CLI_PAGES[0].slug}.md`,
            body: renderCliPage(surface),
        },
    ];

    for (const { path, body } of pages) {
        mkdirSync(dirname(join(root, path)), { recursive: true });
        writeFileSync(join(root, path), body);
    }
    return pages.map(({ path }) => path);
}

function main() {
    // scripts/ -> capsule-docs/ -> repo root
    const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
    const written = generate(root);
    process.stdout.write(
        `gen-reference: wrote ${written.length} page(s)\n${written.map((path) => `  ${path}`).join('\n')}\n`,
    );
}

// Run only when invoked as a script, so the test file can import the renderers.
if (
    process.argv[1] &&
    resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
    main();
}
