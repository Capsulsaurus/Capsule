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

import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { API_GROUPS, CLI_PAGES, groupForPath } from './reference-groups.mjs';

/** Repo-relative path of the committed command-tree artifact. */
export const CLI_SURFACE = 'capsule-cli/cli-surface.json';

/** Command-tree schema version this script understands. */
const CLI_SCHEMA = 1;

/** Repo-relative path of the committed Kynos OpenAPI document. */
export const OPENAPI_DOCUMENT = 'capsule-server/openapi.json';

/**
 * OpenAPI major.minor this generator renders.
 *
 * Pinned rather than accepted loosely because `AGENTS.md` requires the served document to be
 * 3.2 and forbids emitting a 3.1 or 3.0 one: a document that arrived at 3.1 would mean the
 * emitter regressed, and rendering it anyway would publish the regression as documentation.
 */
const OPENAPI_VERSION = '3.2';

/** Repo-relative directory the generated CLI pages are written to. */
const CLI_OUT = 'capsule-docs/src/content/docs/reference/cli';

/** Repo-relative directory the generated REST pages are written to. */
const API_OUT = 'capsule-docs/src/content/docs/reference/api';

/** HTTP methods a path item may carry. Anything else in a path item is not an operation. */
const METHODS = [
    'get',
    'put',
    'post',
    'delete',
    'options',
    'head',
    'patch',
    'trace',
];

/** How deep a `$ref` chain is followed before deeper types become anchor links only. */
const MAX_SCHEMA_DEPTH = 2;

/** The banner every generated page carries, as an HTML comment and as prose. */
const GENERATED_BY = 'capsule-docs/scripts/gen-reference.mjs';

/** A line that opens a fenced block: ``` or ~~~ with any info string. */
const FENCE_OPEN = /^\s*(`{3,}|~{3,})/;

/** A line that *closes* one: the same run with nothing after it but whitespace. */
const FENCE_CLOSE = /^\s*(`{3,}|~{3,})\s*$/;

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
 * **ATX only.** A setext heading (`Title` over `=====`) is left alone. Neither committed
 * artifact uses one — verified across all 971 descriptions in the OpenAPI document — and
 * rewriting a line based on the line below it is a different and more fragile
 * transformation than prefixing hashes: a `---` under a paragraph is a thematic break, and
 * over one it is frontmatter. The limitation is tested, so it fails visibly if it stops
 * being acceptable.
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
            const fenceMatch = FENCE_OPEN.exec(line);
            if (fence === null) {
                if (fenceMatch) {
                    fence = {
                        char: fenceMatch[1][0],
                        length: fenceMatch[1].length,
                    };
                    return line;
                }
            } else {
                // A *closing* fence carries no info string. Without that anchor a
                // ```` ```js ```` line nested inside a ```` ```sh ```` example closes the
                // block early, which both demotes the `#` comments inside the example and
                // leaves every real heading after it untouched.
                const closer = FENCE_CLOSE.exec(line);
                if (
                    closer &&
                    closer[1][0] === fence.char &&
                    closer[1].length >= fence.length
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

/** Where the site's content lives, for turning a repo path into a route. */
const SITE_CONTENT = 'capsule-docs/src/content/docs';

/**
 * Rewrite links inside artifact prose so they mean the same thing on the site.
 *
 * The prose in both artifacts is written in its own context — a Rust doc comment or a `clap`
 * annotation — and is republished here in another. Two link forms travel badly, and both are
 * live in the committed OpenAPI document:
 *
 *   1. **A repo-relative path to a design document.** `[chunk contract](../../../capsule-docs/…/upload-protocol.md)`
 *      resolves from the crate source and from nowhere on the site. It has an exact
 *      equivalent — the Starlight route the same file serves — so it is rewritten, not
 *      dropped: the reader keeps the reference.
 *   2. **A rustdoc intra-doc link.** ``[`revoke_all_signing_bytes`](capsule_core::crypto::revoke::revoke_all_signing_bytes)``
 *      is a path rustdoc resolves and no web server does. There is no equivalent, so the
 *      link is dropped and its text kept.
 *
 * Absolute URLs, site routes, and anchors are left alone.
 *
 * The alternative was to fix the annotations in `capsule-server`, which rule 2 would normally
 * demand. It is the wrong fix here: those links are correct for rustdoc, which is also a
 * published surface, and "correct in the crate, wrong on the site" is a property of
 * republishing rather than an error in the source.
 *
 * @param {string} markdown Prose from an artifact.
 * @returns {string} The prose with its links made meaningful on the site.
 */
function rewriteLinks(markdown) {
    return markdown.replace(
        /(!?\[)([^\]]*)(\]\(\s*)([^)\s]+)(\s*\))/g,
        (whole, open, text, mid, target, close) => {
            if (/^(?:[a-z][a-z0-9+.-]*:|\/|#)/i.test(target)) return whole;
            const normalized = target.replace(/^(?:\.\.\/)+/, '');
            if (
                normalized.startsWith(`${SITE_CONTENT}/`) &&
                normalized.endsWith('.md')
            ) {
                const route = normalized
                    .slice(`${SITE_CONTENT}/`.length)
                    .replace(/(?:\/index)?\.md$/, '');
                return `${open}${text}${mid}/${route}/${close}`;
            }
            // No equivalent: keep the words, drop the link.
            return text;
        },
    );
}

/**
 * Escape a string for a Markdown table cell: a literal `|` would otherwise open a new
 * column, and a newline would end the row.
 *
 * @param {string} text
 * @returns {string}
 */
function cell(text) {
    return (
        rewriteLinks(text)
            .replace(/\s*\n\s*/g, ' ')
            .replace(/\|/g, '\\|')
            // Markdown passes raw HTML through, so an angle-bracketed placeholder — the most
            // likely idiom there is in help text for a command line — parses as a tag and
            // disappears from the rendered page, taking everything up to the next `>` with
            // it if it never closes.
            .replace(/</g, '&lt;')
            .trim()
    );
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
        // Every whitespace run, not just newlines: a stray tab or carriage return inside a
        // double-quoted scalar is a parse hazard, and the value is a one-line description.
        .replace(/\s+/g, ' ')
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
    if (typeof surface.name !== 'string' || surface.name === '') {
        throw new Error(
            `${CLI_SURFACE} carries no command name. An emitter regression producing a ` +
                'well-formed but empty document would otherwise publish a page with an empty ' +
                'heading and a stable badge — the confidently-wrong page this generator ' +
                'exists to make impossible.',
        );
    }
    if (
        !Array.isArray(surface.subcommands) ||
        surface.subcommands.length === 0
    ) {
        throw new Error(
            `${CLI_SURFACE} describes no subcommands. \`capsule\` has several; a document ` +
                'saying otherwise is a regression in the emitter, not a CLI that shrank.',
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
    // Not appended when the help already says it, or `capsule cull --pick` reads
    // "Flag an asset as a keeper (repeatable). Repeatable."
    if (arg.repeatable && !/repeatable/i.test(help ?? '')) {
        parts.push('Repeatable.');
    }
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
 * Collapse runs of blank lines outside fenced blocks, and trim the trailing one.
 *
 * Section assembly leaves a blank run wherever a table ended one block and a heading opened
 * the next. Markdown does not care; a reader diffing two generations does, and so does
 * markdownlint on any machine that has built the site. Applied outside fences only, because
 * a blank run *inside* an example is part of the example — a whole-document regex would have
 * the generator quietly editing the code it is quoting.
 *
 * @param {string} markdown
 * @returns {string}
 */
function tidyBlankLines(markdown) {
    let fence = null;
    const out = [];
    for (const line of markdown.split('\n')) {
        if (fence === null) {
            const open = FENCE_OPEN.exec(line);
            if (open) {
                fence = { char: open[1][0], length: open[1].length };
            } else if (
                line.trim() === '' &&
                out.length > 0 &&
                out[out.length - 1].trim() === ''
            ) {
                continue;
            }
        } else {
            const closer = FENCE_CLOSE.exec(line);
            if (
                closer &&
                closer[1][0] === fence.char &&
                closer[1].length >= fence.length
            ) {
                fence = null;
            }
        }
        out.push(line);
    }
    return out.join('\n').trimEnd();
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
    // Required options are spelled out rather than folded into `[OPTIONS]`. `capsule import`
    // requires `--library <PATH>`, and a usage line that hides it hands the reader a command
    // that fails to parse — a reference page that is wrong, which is the one thing this
    // pipeline exists to prevent.
    for (const option of options.filter((candidate) => candidate.required)) {
        usage.push(spell(option));
    }
    if (options.some((option) => !option.required)) usage.push('[OPTIONS]');
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
        sections.push(demoteHeadings(rewriteLinks(prose), level), '');
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
        `title: ${yamlString(page.label)}`,
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
        tidyBlankLines(renderCommand(surface, [surface.name], 2)),
    ].join('\n')}\n`;
}

/**
 * The committed Kynos OpenAPI document.
 *
 * @param {string} root Repository root.
 * @returns {Record<string, any>}
 */
export function readOpenApiDocument(root) {
    const document = readArtifact(root, OPENAPI_DOCUMENT);
    const version = String(document?.openapi ?? '');
    if (!version.startsWith(`${OPENAPI_VERSION}.`)) {
        throw new Error(
            `${OPENAPI_DOCUMENT} declares OpenAPI ${version || '(nothing)'}, and this ` +
                `generator renders ${OPENAPI_VERSION}. The served document is pinned to ` +
                `${OPENAPI_VERSION} with \`openapi_as(SpecVersion::V3_2)\`; a lower version ` +
                'means the emitter regressed, and rendering it would publish the regression.',
        );
    }
    if (!document.paths || typeof document.paths !== 'object') {
        throw new Error(`${OPENAPI_DOCUMENT} declares no paths.`);
    }
    return document;
}

/**
 * Bucket every operation in the document into its group, in a stable order.
 *
 * **Fails on an operation no group claims.** That is the whole value of a hand-curated
 * table: a new endpoint family cannot publish under a heading nobody chose, and — the case
 * that actually bites — cannot silently fail to publish at all while the build stays green.
 *
 * @param {Record<string, any>} document The parsed OpenAPI document.
 * @returns {Map<string, Array<{ path: string, method: string, operation: Record<string, any>, operationId?: string }>>}
 *   Keyed by group slug, in `API_GROUPS` order, with every group present.
 */
export function bucketOperations(document) {
    /** @type {Map<string, any[]>} */
    const buckets = new Map(API_GROUPS.map((group) => [group.slug, []]));

    // Sorted rather than taken in document order: JSON object order is an emitter detail,
    // and a page whose sections reshuffle when the server's route registration is reordered
    // produces a diff nobody can read.
    for (const path of Object.keys(document.paths).sort()) {
        const group = groupForPath(path);
        const item = document.paths[path] ?? {};
        const methods = METHODS.filter((method) => item[method]);
        if (!group) {
            const named = methods
                .map((method) => `${method.toUpperCase()} ${path}`)
                .join(', ');
            throw new Error(
                `no reference group claims ${named || path}. Add its prefix to a group in ` +
                    'capsule-docs/scripts/reference-groups.mjs — an endpoint family must not ' +
                    'publish under a heading nobody chose, and must not silently fail to publish.',
            );
        }
        for (const method of methods) {
            buckets.get(group.slug).push({
                path,
                method: method.toUpperCase(),
                operation: item[method],
                operationId: item[method].operationId,
            });
        }
    }
    return buckets;
}

/**
 * Render a schema's type as a short string: `string`, `string | null`, `integer[]`, or the
 * name of a referenced schema.
 *
 * @param {Record<string, any>} schema
 * @returns {string}
 */
function typeOf(schema) {
    if (!schema || typeof schema !== 'object') return 'unknown';
    if (schema.$ref) return refName(schema.$ref);
    if (schema.type === 'array') {
        return `${typeOf(schema.items ?? {})}[]`;
    }
    const type = schema.type;
    // OpenAPI 3.1 and later spell nullability as a type union rather than as `nullable`, so
    // `type` is an array here and interpolating it directly yields `string,null`.
    if (Array.isArray(type)) return type.join(' | ');
    if (typeof type === 'string') return type;
    if (schema.enum) return 'string';
    return 'object';
}

/**
 * The schema name a local `$ref` points at.
 *
 * @param {string} ref
 * @returns {string}
 */
function refName(ref) {
    return ref.split('/').pop();
}

/**
 * The set of schema names a page must document, walked from its operations to
 * `MAX_SCHEMA_DEPTH`.
 *
 * Depth-bounded rather than exhaustive, and visited-set guarded, so a self-referential or
 * mutually-referential schema cannot spin: the current document has no cycle, but a renderer
 * that would hang on one is a renderer that fails the day someone adds a tree.
 *
 * @param {Record<string, any>} document
 * @param {Array<{ operation: Record<string, any> }>} operations
 * @returns {string[]} Schema names, sorted.
 */
function schemasUsedBy(document, operations) {
    const seen = new Set();

    const visit = (schema, depth) => {
        if (!schema || typeof schema !== 'object' || depth > MAX_SCHEMA_DEPTH)
            return;
        if (Array.isArray(schema)) {
            for (const entry of schema) visit(entry, depth);
            return;
        }
        if (schema.$ref) {
            const name = refName(schema.$ref);
            if (seen.has(name)) return;
            seen.add(name);
            visit(document.components?.schemas?.[name], depth + 1);
            return;
        }
        for (const value of Object.values(schema)) visit(value, depth);
    };

    for (const { operation } of operations) {
        visit(operation.requestBody ?? {}, 0);
        visit(operation.responses ?? {}, 0);
        visit(operation.parameters ?? [], 0);
    }
    return [...seen].sort();
}

/**
 * The media type and schema of a request or response body, or null when it carries none.
 *
 * @param {Record<string, any> | undefined} carrier
 * @returns {{ mediaType: string, schema: Record<string, any> } | null}
 */
function bodyOf(carrier) {
    const content = carrier?.content;
    if (!content) return null;
    const mediaType = Object.keys(content).sort()[0];
    if (!mediaType) return null;
    return { mediaType, schema: content[mediaType]?.schema ?? {} };
}

/**
 * A schema reference rendered as a link into this page's appendix, when the appendix
 * documents it, and as bare code when it does not.
 *
 * @param {string[]} documented Schema names the page's appendix carries.
 * @param {Record<string, any>} schema
 * @returns {string}
 */
function schemaLink(documented, schema) {
    const rendered = typeOf(schema);
    const bare = rendered.replace(/\[\]$/, '');
    // A nullable type renders as `string | null`, and every use of this is a table cell, so
    // the separator has to be escaped or it opens a fourth column and shifts the row.
    // GFM requires the escape inside a code span too, and renders it as a bare pipe.
    const shown = rendered.replace(/\|/g, '\\|');
    return documented.includes(bare)
        ? `[\`${shown}\`](#${bare.toLowerCase()})`
        : `\`${shown}\``;
}

/**
 * Render one operation.
 *
 * @param {{ path: string, method: string, operation: Record<string, any> }} entry
 * @param {string[]} documented Schema names the page's appendix carries.
 * @returns {string}
 */
function renderOperation({ path, method, operation }, documented) {
    const sections = [`## ${method} ${path}`];

    if (operation.summary) {
        sections.push(demoteHeadings(rewriteLinks(operation.summary), 2));
    }
    if (operation.description) {
        // Demoted by 3: an operation is an h2, so a description opening at `#` becomes an
        // h4 under it rather than a second page title.
        sections.push(demoteHeadings(rewriteLinks(operation.description), 3));
    }

    const security = operation.security;
    if (Array.isArray(security) && security.length > 0) {
        const schemes = security
            .flatMap((requirement) => Object.keys(requirement))
            .sort();
        sections.push(
            `**Authentication:** required — ${schemes.map((scheme) => `\`${scheme}\``).join(', ')}.`,
        );
    } else {
        sections.push('**Authentication:** none.');
    }

    const parameters = operation.parameters ?? [];
    if (parameters.length > 0) {
        sections.push(
            table(
                ['Parameter', 'In', 'Type', 'Description'],
                [...parameters]
                    .sort(
                        (a, b) =>
                            a.in.localeCompare(b.in) ||
                            a.name.localeCompare(b.name),
                    )
                    .map((parameter) => [
                        `\`${parameter.name}\``,
                        `\`${parameter.in}\``,
                        schemaLink(documented, parameter.schema ?? {}),
                        [
                            parameter.required ? '**Required.**' : '',
                            parameter.description
                                ? sentence(cell(parameter.description))
                                : '',
                            parameter.example === undefined
                                ? ''
                                : `Example: \`${parameter.example}\`.`,
                        ]
                            .filter(Boolean)
                            .join(' ') || '—',
                    ]),
            ),
        );
    }

    const requestBody = bodyOf(operation.requestBody);
    if (requestBody) {
        sections.push(
            `**Request body** (${operation.requestBody.required ? 'required' : 'optional'}, ` +
                `\`${requestBody.mediaType}\`): ${schemaLink(documented, requestBody.schema)}`,
        );
    }

    const responses = Object.entries(operation.responses ?? {}).sort(
        ([a], [b]) => Number(a) - Number(b) || a.localeCompare(b),
    );
    if (responses.length > 0) {
        sections.push(
            table(
                ['Status', 'Body', 'Description'],
                responses.map(([status, response]) => {
                    const body = bodyOf(response);
                    const headers = Object.keys(response.headers ?? {}).sort();
                    return [
                        `\`${status}\``,
                        body
                            ? `${schemaLink(documented, body.schema)} <br /> \`${body.mediaType}\``
                            : '—',
                        [
                            response.description
                                ? sentence(cell(response.description))
                                : '',
                            headers.length > 0
                                ? `Headers: ${headers.map((header) => `\`${header}\``).join(', ')}.`
                                : '',
                        ]
                            .filter(Boolean)
                            .join(' ') || '—',
                    ];
                }),
            ),
        );
    }

    return sections.filter(Boolean).join('\n\n');
}

/**
 * Render one schema as an appendix entry.
 *
 * @param {string} name
 * @param {Record<string, any>} document The document, for the schema's own definition.
 * @param {string[]} documented Every schema name this page's appendix carries.
 * @returns {string}
 */
function renderSchema(name, document, documented) {
    const schema = document.components?.schemas?.[name] ?? {};
    const sections = [`### ${name}`];

    if (schema.description)
        sections.push(demoteHeadings(schema.description, 3));

    if (schema.enum) {
        sections.push(
            `One of: ${schema.enum.map((value) => `\`${value}\``).join(', ')}.`,
        );
        return sections.join('\n\n');
    }

    const required = new Set(schema.required ?? []);
    const properties = Object.entries(schema.properties ?? {});
    if (properties.length === 0) {
        sections.push(`Type: \`${typeOf(schema)}\`.`);
        return sections.join('\n\n');
    }

    sections.push(
        table(
            ['Field', 'Type', 'Description'],
            properties.map(([field, property]) => [
                `\`${field}\``,
                schemaLink(documented, property),
                [
                    required.has(field) ? '**Required.**' : '',
                    property.description
                        ? sentence(cell(property.description))
                        : '',
                ]
                    .filter(Boolean)
                    .join(' ') || '—',
            ]),
        ),
    );
    return sections.join('\n\n');
}

/**
 * The generated `/reference/api/<group>/` page.
 *
 * @param {import('./reference-groups.mjs').ApiGroup} group
 * @param {Array<{ path: string, method: string, operation: Record<string, any> }>} operations
 * @param {Record<string, any>} document
 * @returns {string} Markdown, frontmatter included.
 */
export function renderApiPage(group, operations, document) {
    const documented = schemasUsedBy(document, operations);

    const head = [
        '---',
        `title: ${yamlString(group.label)}`,
        `description: ${yamlString(group.description)}`,
        'status: stable',
        '---',
        '',
        `<!-- Generated from ${OPENAPI_DOCUMENT} by ${GENERATED_BY}. Do not edit. -->`,
        '',
        `Generated from \`${OPENAPI_DOCUMENT}\`, the OpenAPI ${OPENAPI_VERSION} document`,
        '`capsule-server` emits and `mise run openapi-check-kynos` keeps current. To change a',
        'description on this page, change the annotation on the handler or model it comes from',
        'and regenerate — this file is build output. The auth model, error contract, and',
        'conventions common to every endpoint are on the [REST API overview](/reference/api/).',
    ].join('\n');

    const body = operations
        .map((entry) => renderOperation(entry, documented))
        .join('\n\n');

    const appendix =
        documented.length === 0
            ? ''
            : [
                  '## Schemas',
                  '',
                  'The models these endpoints carry. A field whose type names another model links',
                  'to it; a model reached more than two references deep is named without being',
                  'expanded here.',
                  '',
                  documented
                      .map((name) => renderSchema(name, document, documented))
                      .join('\n\n'),
              ].join('\n');

    return `${tidyBlankLines([head, body, appendix].filter(Boolean).join('\n\n'))}\n`;
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
    const document = readOpenApiDocument(root);
    const buckets = bucketOperations(document);

    /** @type {Array<{ path: string, body: string }>} */
    const pages = [
        {
            path: `${CLI_OUT}/${CLI_PAGES[0].slug}.md`,
            body: renderCliPage(surface),
        },
        ...API_GROUPS.map((group) => ({
            path: `${API_OUT}/${group.slug}.md`,
            body: renderApiPage(group, buckets.get(group.slug) ?? [], document),
        })),
    ];

    // Cleared, not merged into. A page this run no longer emits — a group renamed, a surface
    // dropped — would otherwise stay on disk, and because the directory is gitignored
    // `git status` never shows it: Astro would keep routing, indexing, and link-validating a
    // page no artifact describes, on this machine and on no CI runner. Only the generated
    // directories, so the hand-written overviews beside them survive.
    for (const directory of [CLI_OUT, API_OUT]) {
        rmSync(join(root, directory), { recursive: true, force: true });
    }

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
