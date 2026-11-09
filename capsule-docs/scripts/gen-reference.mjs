#!/usr/bin/env node

/**
 * gen-reference — emit `/reference/` from the committed description artifacts.
 *
 * How a generated page stays true to the code:
 *
 *   1. Each surface's own toolchain emits a committed artifact (`openapi.json`,
 *      `cli-surface.json`, `bindings-surface.json`, `wasm-surface.d.ts`).
 *   2. A required CI gate regenerates it from the code and fails on any byte difference, so
 *      `master` never holds a stale one; `cargo test` does the same for the REST and CLI
 *      artifacts, and a dev run warns when a branch changed an artifact's source but not it.
 *   3. This script reads only those artifacts, before every `astro dev` and `astro build`.
 *   4. Each page names its artifact in the banner under its title.
 *
 * `design/developer-docs.md` fixes the boundary at *artifacts, not toolchains*: the CI
 * `docs` job installs bun and nothing else, so this script may never shell out to cargo. It
 * reads committed JSON and writes Markdown, which is the whole of its contract.
 *
 * The pages it writes are ordinary content-collection entries under
 * `src/content/docs/reference/`, which is what buys the rest for free — Pagefind indexes
 * them, `starlight-links-validator` checks their links, the `PageTitle` override renders
 * their generated-page banner, and the notranslate rehype pass marks up their technical terms. A
 * mounted OpenAPI application would have had none of that.
 *
 * They are also **gitignored**, and that is the point of rule 2: a generated page is never
 * edited, so committing one only creates a copy that can disagree with its source. Fix the
 * clap `about` or the schema description and regenerate.
 *
 * Seven failure modes are deliberately fatal rather than degraded, because
 * `developer-docs.md` calls a stale reference page worse than a missing one — a missing page
 * is obvious and a wrong one is believed:
 *
 *   1. an artifact is absent or unparseable — exit naming the path;
 *   2. an artifact's `schema` is one this script was not written against — exit rather than
 *      render a half-understood document;
 *   3. an operation matches no group in `reference-groups.mjs` — exit naming it, so a new
 *      endpoint family cannot publish unlisted;
 *   4. a request or response carrier offers **more than one media type** — this renderer
 *      shows one body per carrier, so a second would be dropped silently and the page would
 *      claim an endpoint accepts only JSON when it also accepts CBOR;
 *   5. a schema composes with **`oneOf`, `allOf`, or `anyOf`** — this renderer flattens a
 *      schema to a property table, which cannot express a union or an intersection, and
 *      would render one as an empty or a half-true model;
 *   6. artifact prose carries a **setext heading** — see [`demoteHeadings`];
 *   7. an SDK artifact carries a declaration its reader does not know — a line of the wasm
 *      digest outside wasm-bindgen's layout, or a binding declaration kind no page section
 *      lists — exit naming it, so a new kind of export cannot publish unlisted.
 *
 * Modes 4 to 7 are unreachable on the committed artifacts today. They are fatal rather
 * than deferred because each is a case where the renderer's *silent* answer is a confident
 * lie, and a build failure naming the operation is how the first one to appear gets
 * handled instead of shipped.
 *
 * Usage: `bun capsule-docs/scripts/gen-reference.mjs` from anywhere; `package.json` runs it
 * before `astro dev` and `astro build`.
 */

import {
    existsSync,
    mkdirSync,
    readdirSync,
    readFileSync,
    rmSync,
    writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { slugify } from './lib/markdown.mjs';
import {
    ARTIFACT_SOURCES,
    mtimes,
    readChanges,
    skipReason,
    staleArtifacts,
} from './lib/staleness.mjs';
import {
    API_CONVENTIONS,
    API_GROUPS,
    CLI_PAGES,
    groupForPath,
    SDK_PAGES,
} from './reference-groups.mjs';

/** Repo-relative path of the committed command-tree artifact. */
export const CLI_SURFACE = 'capsule-cli/cli-surface.json';

/** Command-tree schema version this script understands. */
const CLI_SCHEMA = 1;

/** Repo-relative path of the committed Kynos OpenAPI document. */
export const OPENAPI_DOCUMENT = 'capsule-server/openapi.json';

/**
 * OpenAPI major.minor this generator renders.
 *
 * Pinned rather than accepted loosely because `.claude/rules/capsule-server.md` requires the served document to be
 * 3.2 and forbids emitting a 3.1 or 3.0 one: a document that arrived at 3.1 would mean the
 * emitter regressed, and rendering it anyway would publish the regression as documentation.
 */
const OPENAPI_VERSION = '3.2';

/** Repo-relative path of the committed uniffi binding surface dump. */
export const BINDINGS_SURFACE = 'capsule-sdk/bindings-surface.json';

/** Binding-dump schema version this script understands. */
const BINDINGS_SCHEMA = 1;

/** Repo-relative path of the committed `capsule_wasm.d.ts` surface digest. */
export const WASM_SURFACE = 'capsule-wasm/wasm-surface.d.ts';

/** Repo-relative directory the generated CLI pages are written to. */
const CLI_OUT = 'capsule-docs/src/content/docs/reference/cli';

/** Repo-relative directory the generated REST pages are written to. */
const API_OUT = 'capsule-docs/src/content/docs/reference/api';

/**
 * Repo-relative directory the generated SDK pages are written to. Unlike the CLI and REST
 * directories it also holds one hand-written page — see `SDK_PAGES` — so it is cleared file
 * by file rather than as a whole.
 */
const SDK_OUT = 'capsule-docs/src/content/docs/reference/sdk';

/**
 * The `generated` frontmatter block (src/content.config.ts) the page banner renders: which
 * surface the page documents, what it was generated from, and where to download that.
 *
 * @param {string} surface
 * @param {string} source
 * @param {string} [download]
 * @returns {string[]} Frontmatter lines.
 */
function generatedFrontmatter(surface, source, download) {
    return [
        'generated:',
        `  surface: ${yamlString(surface)}`,
        `  source: ${yamlString(source)}`,
        ...(download ? [`  download: ${yamlString(download)}`] : []),
    ];
}

/** Repo-relative directory Astro serves verbatim at the site root. */
const PUBLIC_OUT = 'capsule-docs/public';

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

/**
 * How deep a `$ref` chain is followed before deeper models are named but not expanded.
 *
 * Set above the committed document's needs, not at them. The deepest chain any group
 * reaches is 3 — `SyncPageResponse` → `SyncEntry` → `SyncBlobRef` → `WireBlobRole`, on
 * `/reference/api/sync/` — so at the previous value of 2 that enum was named on the page
 * and defined nowhere. Four documents the complete closure of every group today with a
 * level of headroom; the measured cost of the whole closure over the bounded walk is one
 * additional schema across all eleven pages.
 *
 * A model reachable only deeper than this is still *named* on the page, as bare code rather
 * than a link, so the bound degrades to "less detail" and never to a link that goes nowhere.
 * That is why exceeding it is not fatal, unlike the cases in the module header.
 */
const MAX_SCHEMA_DEPTH = 4;

/** The banner every generated page carries, as an HTML comment and as prose. */
const GENERATED_BY = 'capsule-docs/scripts/gen-reference.mjs';

/** A line that opens a fenced block: ``` or ~~~ with any info string. */
const FENCE_OPEN = /^\s*(`{3,}|~{3,})/;

/** A line that *closes* one: the same run with nothing after it but whitespace. */
const FENCE_CLOSE = /^\s*(`{3,}|~{3,})\s*$/;

/**
 * A setext underline: a run of `=` or `-` alone on its line.
 *
 * **One character is enough.** CommonMark's `setext heading underline` is `= +` or `- +`,
 * not two or more, so `Title` over a lone `-` is an h2 exactly as `Title` over `---` is.
 * Requiring two let a single-character underline through the check that exists to catch it,
 * which shipped an undemoted heading into a page body — the defect, arriving through the
 * guard against the defect.
 *
 * Up to three leading spaces, and trailing whitespace, both per the spec.
 */
const SETEXT_UNDERLINE = /^\s{0,3}(={1,}|-{1,})\s*$/;

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
 * **ATX only, and a setext heading is fatal.** Rewriting a line based on the line below it
 * is a different and more fragile transformation than prefixing hashes — a `---` under a
 * paragraph is a thematic break, and over one it is frontmatter — so this function does not
 * attempt it. Silently leaving one alone is worse than not supporting it: an `=====`
 * underline in an operation description would put an undemoted h1 in the page body, which
 * is the exact defect demotion exists to prevent, and it would publish looking fine.
 * Neither committed artifact uses one today (verified across all 971 descriptions in the
 * OpenAPI document), so the first one to appear stops the build and gets ATX in its doc
 * comment.
 *
 * @param {string} markdown Prose that may contain headings.
 * @param {number} offset Levels to add.
 * @returns {string} The prose with its headings demoted.
 */
export function demoteHeadings(markdown, offset) {
    let fence = null;
    /** @type {string[]} */
    const lines = [];
    /** Indices of lines that sat inside a fenced block, which is code, not prose. */
    const fenced = new Set();
    markdown.split('\n').forEach((line) => {
        const fenceMatch = FENCE_OPEN.exec(line);
        if (fence === null) {
            if (fenceMatch) {
                fence = {
                    char: fenceMatch[1][0],
                    length: fenceMatch[1].length,
                };
                lines.push(line);
                fenced.add(lines.length - 1);
                return;
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
            // Inside a fence, and pushed with a marker the setext scan below reads as
            // "not prose": an `=====` in a code example is code.
            lines.push(line);
            fenced.add(lines.length - 1);
            return;
        }
        const heading = /^(#{1,6})(\s)/.exec(line);
        if (!heading) {
            lines.push(line);
            return;
        }
        const level = Math.min(6, heading[1].length + offset);
        lines.push('#'.repeat(level) + line.slice(heading[1].length));
    });

    // Checked after the pass so the fence state above decides what is prose. A setext
    // underline is a run of `=` or `-` alone on a line, directly under a non-blank one that
    // is not itself a heading, a list item, or a table row.
    for (let i = 1; i < lines.length; i += 1) {
        if (fenced.has(i) || fenced.has(i - 1)) continue;
        if (!SETEXT_UNDERLINE.test(lines[i])) continue;
        const above = lines[i - 1];
        if (above.trim() === '') continue;
        if (/^\s*(#{1,6}\s|[-*+>|]|\d+[.)]\s)/.test(above)) continue;
        throw new Error(
            `artifact prose carries a setext heading ("${above.trim()}" underlined with ` +
                `"${lines[i].trim()}"). This generator demotes ATX headings only, and an ` +
                'undemoted heading in a page body is the defect demotion exists to prevent. ' +
                'Rewrite it as an ATX heading (`## Title`) in the doc comment it comes from.',
        );
    }

    return lines.join('\n');
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
 *   1. **A repo-relative path to a design document.** `[chunk contract](../../../capsule-docs/…/transfer/upload.md)`
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
 * Prepare artifact prose for a one-line context: rewrite its links, flatten it to a single
 * line. Deliberately does **not** escape — see [`escapeCell`].
 *
 * @param {string} text
 * @returns {string}
 */
function prose(text) {
    return rewriteLinks(text)
        .replace(/\s*\n\s*/g, ' ')
        .trim();
}

/**
 * Escape one **finished** table cell — after every fragment that composes it has been
 * assembled, never fragment by fragment.
 *
 * That ordering is the whole point. A cell is built from several sources — the help text,
 * then `Values: …`, `Default: …`, `Example: …` appended after it — and escaping only the
 * first leaves the others raw. A default of `a|b` then opens a column of its own and shifts
 * every cell to its right, which is exactly the defect this function exists to prevent and
 * exactly the one a per-fragment escape reintroduces.
 *
 * Two characters are escaped, and only these two:
 *
 * - `|`, which opens a column. GFM requires the escape inside a code span too, and renders
 *   it as a bare pipe, so `` `a\|b` `` shows the pipe the artifact meant.
 * - `<`, because Markdown passes raw HTML through. `<PATH>` — the most likely idiom in help
 *   text for a command line — parses as a tag and vanishes, taking everything up to the
 *   next `>` with it if it never closes.
 *
 * **Artifact prose is trusted Markdown.** It comes from Rust doc comments and `clap`
 * annotations in this repository, reviewed like any other source, so emphasis, links, and
 * inline code in it are the author's intent and are passed through rather than sanitized.
 * These two escapes are not a security boundary; they repair characters whose meaning
 * *changes* when prose written for a doc comment is republished inside a table. An unclosed
 * `<` outside a code span in a doc comment is the author's bug, and it is fixed in the doc
 * comment.
 *
 * Never apply this to generator-authored markup: the `<br />` in a response body cell is
 * markup this file wrote and means to keep.
 *
 * @param {string} text A fully assembled cell.
 * @returns {string}
 */
function escapeCell(text) {
    return escapePipes(text).replace(/</g, '&lt;').trim();
}

/**
 * Escape every pipe in `text` that is not already escaped, so none of them opens a column.
 *
 * A backslash escape is read as the pair it is — a backslash and the character after it —
 * and passed through whole, exactly as the GFM table tokenizer reads it. Escaping a bare `|`
 * alone is not enough: in `a\|b` the tokenizer pairs the existing backslash with the one the
 * escape adds, `a\\|b` reads as an escaped backslash followed by a column separator, and the
 * row gains a cell. Escaping every backslash instead would be wrong the other way: artifact
 * prose is Markdown, so its `\*` and `\|` are the author's escapes and must survive.
 *
 * @param {string} text Markdown cell content.
 * @returns {string}
 */
function escapePipes(text) {
    return text.replace(/\\[\s\S]|\|/g, (token) =>
        token === '|' ? '\\|' : token,
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
 * Read an artifact's text, naming its emitter when it is absent.
 *
 * @param {string} root Repository root.
 * @param {string} relPath Repo-relative artifact path.
 * @returns {string}
 */
function readArtifactText(root, relPath) {
    try {
        return readFileSync(join(root, relPath), 'utf8');
    } catch (cause) {
        throw new Error(
            `cannot read the description artifact ${relPath}: ${cause.message}. ` +
                'Run its emitter (`mise run cli-surface`, `mise run openapi-kynos`, ' +
                '`mise run gen-bindings`, `mise run wasm-surface`) and commit the result.',
            { cause },
        );
    }
}

/**
 * Read and validate a JSON artifact.
 *
 * @param {string} root Repository root.
 * @param {string} relPath Repo-relative artifact path.
 * @returns {unknown}
 */
function readArtifact(root, relPath) {
    const raw = readArtifactText(root, relPath);
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
 * One argument's man-page entry: whether it is required and repeatable, what it does,
 * what it accepts, and what it defaults to.
 *
 * @param {Record<string, any>} arg
 * @returns {string}
 */
function describeArg(arg) {
    const parts = [];
    if (arg.required) parts.push('**Required.**');
    const help = arg.long_help ?? arg.help;
    if (help) parts.push(sentence(prose(help)));
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
    // One pass, over the assembled entry: a `<` in a default or an enumerated value is as
    // capable of being read as a tag as one in the help text.
    return inlineProse(parts.join(' ')) || '—';
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

    // Laid out as a man page, the form a command-line user already reads: the command's own
    // heading (so search and the table of contents still reach it), then SYNOPSIS,
    // DESCRIPTION, ARGUMENTS, OPTIONS and COMMANDS, each option a hanging-indent entry.
    const sections = [
        `${'#'.repeat(level)} ${invocation}`,
        '<p class="man-label">Synopsis</p>',
        `\`\`\`text\n${usage.join(' ')}\n\`\`\``,
    ];

    // Demoted relative to this command's own heading, so a doc comment that opens at `#`
    // nests under the command it describes instead of outranking it.
    const about = command.long_about ?? command.about;
    if (about) {
        sections.push(
            '<p class="man-label">Description</p>',
            demoteHeadings(rewriteLinks(about), level),
        );
    }

    for (const [label, entries] of [
        ['Arguments', positionals],
        ['Options', options],
    ]) {
        if (entries.length === 0) continue;
        sections.push(
            `<p class="man-label">${label}</p>`,
            `<div class="man-options">\n\n${entries
                .map((arg) => `- \`${spell(arg)}\`\n\n  ${describeArg(arg)}`)
                .join('\n\n')}\n\n</div>`,
        );
    }

    if (subcommands.length > 0) {
        sections.push(
            '<p class="man-label">Commands</p>',
            `<div class="man-options">\n\n${subcommands
                .map((subcommand) => {
                    const words = [...path, subcommand.name];
                    const link = `[\`${words.join(' ')}\`](#${words.join('-')})`;
                    return subcommand.about
                        ? `- ${link}\n\n  ${inlineProse(prose(subcommand.about))}`
                        : `- ${link}`;
                })
                .join('\n\n')}\n\n</div>`,
        );
    }

    return [
        `<section class="man-cmd">\n\n${sections.join('\n\n')}\n\n</section>`,
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
        ...generatedFrontmatter('CLI', CLI_SURFACE),
        '---',
        '',
        `<!-- Generated from ${CLI_SURFACE} by ${GENERATED_BY}. Do not edit. -->`,
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
    assertRenderable(document);
    return document;
}

/** Schema keywords this renderer cannot express. */
const COMPOSITION_KEYWORDS = ['oneOf', 'allOf', 'anyOf'];

/**
 * Fail on anything in the document this renderer would answer wrongly rather than not at
 * all. See the fatal list in the module header for why each is fatal.
 *
 * @param {Record<string, any>} document
 * @throws {Error} naming the operation or the schema.
 */
function assertRenderable(document) {
    for (const [path, item] of Object.entries(document.paths)) {
        for (const method of METHODS) {
            const operation = item?.[method];
            if (!operation) continue;
            const at = `${method.toUpperCase()} ${path}`;

            const carriers = [
                ['request body', operation.requestBody],
                ...Object.entries(operation.responses ?? {}).map(
                    ([status, response]) => [`response ${status}`, response],
                ),
            ];
            for (const [which, carrier] of carriers) {
                const media = Object.keys(carrier?.content ?? {});
                if (media.length > 1) {
                    throw new Error(
                        `${at}: its ${which} offers ${media.length} media types ` +
                            `(${media.sort().join(', ')}), and this generator renders one body ` +
                            'per carrier. Rendering it would document the endpoint as ' +
                            'accepting only the first. Teach ' +
                            `${GENERATED_BY} to render every media type before the server ` +
                            'starts offering a choice.',
                    );
                }
            }
        }
    }

    for (const [name, schema] of Object.entries(
        document.components?.schemas ?? {},
    )) {
        assertNoComposition(name, name, schema);
    }
}

/**
 * The one branch of a nullable union, or `null` when the schema is not one.
 *
 * OpenAPI 3.1 has no `nullable` keyword, so an optional *object* is spelled
 * `anyOf: [{ $ref }, { type: 'null' }]` — which is what every `Option<T>` over a struct
 * emits, `AuthEndpointsResponse.oidc` being the first to reach this generator. That is a
 * composition by the letter of the document and not by intent: there is exactly one real
 * branch, so it flattens to a property table perfectly well once the null arm is dropped.
 *
 * Deliberately narrow. A union with two real branches still has no property table and is
 * still refused by {@link assertNoComposition} — this recognises the nullable idiom only,
 * which is the difference between rendering an `Option` and guessing at a sum type.
 *
 * @param {unknown} schema
 * @returns {Record<string, any> | null} The non-null branch, or `null`.
 */
function nullableBranch(schema) {
    if (!schema || typeof schema !== 'object' || Array.isArray(schema))
        return null;
    for (const word of ['anyOf', 'oneOf']) {
        const branches = schema[word];
        if (!Array.isArray(branches)) continue;
        const real = branches.filter((branch) => branch?.type !== 'null');
        if (real.length === 1 && real.length < branches.length) return real[0];
    }
    return null;
}

/**
 * Refuse `oneOf`/`allOf`/`anyOf` anywhere in a named schema, not only at its root.
 *
 * Checking the root alone is the shape of the bug it was meant to prevent: a composition one
 * level down, in `properties.<name>` or in `items`, falls through `typeOf` to the string
 * `object` and renders as a row claiming the field is a plain object. That is a confident
 * lie about a union, which is exactly what this check exists to stop, and it passed.
 *
 * Only *inline* subschemas are walked. A `$ref` is not followed, because its target is a
 * named schema that this scan reaches on its own pass — following it would report the same
 * composition once per reference, under whichever name happened to be scanned first.
 *
 * @param {string} name The named schema this subtree belongs to.
 * @param {string} at A readable path to the subschema, for the error.
 * @param {unknown} schema The subschema.
 * @throws {Error} naming the schema and the path within it.
 */
function assertNoComposition(name, at, schema) {
    if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return;
    if (schema.$ref) return;

    // An optional object is a composition only by spelling; see `nullableBranch`. Walk into
    // the real branch so a genuine union *inside* an optional is still caught.
    const nullable = nullableBranch(schema);
    if (nullable) {
        assertNoComposition(name, at, nullable);
        return;
    }

    const composed = COMPOSITION_KEYWORDS.filter((word) => schema[word]);
    if (composed.length > 0) {
        throw new Error(
            `schema ${name} composes with ${composed.join(' and ')} at ${at}, which this ` +
                'generator cannot express: it flattens a schema to a property table, ' +
                'and a union or an intersection is not a property table. It would ' +
                `render as an empty or a half-true model. Teach ${GENERATED_BY} to ` +
                'render composition before the server starts emitting it.',
        );
    }

    for (const [property, subschema] of Object.entries(
        schema.properties ?? {},
    )) {
        assertNoComposition(name, `${at}.${property}`, subschema);
    }
    assertNoComposition(name, `${at}[]`, schema.items);
    // `additionalProperties` is a schema when it is an object, and `true`/`false` otherwise.
    if (typeof schema.additionalProperties === 'object') {
        assertNoComposition(
            name,
            `${at}.additionalProperties`,
            schema.additionalProperties,
        );
    }
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
    // `Option<T>` over an object, spelled as a union with `null`. Rendered as the branch it
    // actually carries, so a reader sees `OidcEndpointsResponse | null` rather than `object`.
    const nullable = nullableBranch(schema);
    if (nullable) return `${typeOf(nullable)} | null`;
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
 * Depth-bounded rather than exhaustive so a self-referential or mutually-referential schema
 * cannot spin: the current document has no cycle, but a renderer that would hang on one is
 * a renderer that fails the day someone adds a tree.
 *
 * The bound and the cycle guard interact, which is the subtle part — see the comment on
 * `seen` below. The rule the page states, and the one implemented here, is: a schema is
 * documented when some path reaches it within the bound, whichever path the walk takes
 * first.
 *
 * @param {Record<string, any>} document
 * @param {Array<{ operation: Record<string, any> }>} operations
 * @returns {string[]} Schema names, sorted.
 */
function schemasUsedBy(document, operations) {
    /** @type {Map<string, number>} Schema name -> shallowest depth it was reached at. */
    const seen = new Map();

    const visit = (schema, depth) => {
        if (!schema || typeof schema !== 'object' || depth > MAX_SCHEMA_DEPTH)
            return;
        if (Array.isArray(schema)) {
            for (const entry of schema) visit(entry, depth);
            return;
        }
        if (schema.$ref) {
            const name = refName(schema.$ref);
            // Keyed on the *shallowest* depth this name has been reached at, not on having
            // been seen at all. A plain visited set makes the answer depend on traversal
            // order: reach `SyncEntry` at depth 2 first and its children are cut by the
            // bound, and the later path that reaches it at depth 1 — where its children are
            // in range — is then refused as already-seen. `WireBlobRole` on
            // `/reference/api/sync/` was documented or not according to which operation the
            // walk happened to read first. Re-expanding on a shallower arrival still
            // terminates: a name can only improve `MAX_SCHEMA_DEPTH + 1` times, and a cycle
            // never arrives shallower twice.
            if (seen.has(name) && seen.get(name) <= depth) return;
            seen.set(name, depth);
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
    return [...seen.keys()].sort();
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
    // Exactly one, or none: `assertRenderable` has already refused a carrier offering a
    // choice, so `sort()[0]` is the only entry rather than an arbitrary pick.
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
function schemaLink(documented, schema, { inTable = true } = {}) {
    const rendered = typeOf(schema);
    const bare = rendered.replace(/\[\]$/, '');
    // A nullable type renders as `string | null`; in a table cell the separator has to be
    // escaped or it opens a fourth column and shifts the row. GFM requires the escape inside
    // a code span too, and renders it as a bare pipe. Outside a table the escape would show.
    const shown = inTable ? escapePipes(rendered) : rendered;
    return documented.includes(bare)
        ? `[\`${shown}\`](#${bare.toLowerCase()})`
        : `\`${shown}\``;
}

/** The placeholder origin request examples are written against. */
const EXAMPLE_ORIGIN = 'https://capsule.example';

/** Where `generate` publishes the OpenAPI document beside the site, and the banner links to. */
export const OPENAPI_DOWNLOAD = '/openapi.json';

/**
 * A method rendered as the pill every operation heading and index entry opens with. The
 * colour comes from `data-method` (src/styles/api-reference.css); the text stays the
 * method, so the heading's slug and its plain-text reading are unchanged.
 *
 * @param {string} method Upper-case HTTP method.
 * @returns {string}
 */
function methodPill(method) {
    return `<span class="api-method" data-method="${method.toLowerCase()}">${method}</span>`;
}

/**
 * The anchor an operation heading gets: its text with the pill markup stripped, slugged the
 * way the site slugs every heading.
 *
 * @param {string} method
 * @param {string} path
 * @returns {string}
 */
export function operationAnchor(method, path) {
    return slugify(`${method} ${path}`);
}

/**
 * A deterministic example value for a schema, for the request and response panels.
 *
 * Taken from the document where it says: `example`, then `default`, then the first `enum`
 * member. Otherwise a placeholder of the right shape — a UUIDv7 for `format: uuid`, RFC 3339
 * for date-times, `0`, `true`, `"string"`. References are followed to the same depth the
 * schema appendix expands, so an example never claims more structure than the page
 * documents; past it the value is `{}`.
 *
 * @param {Record<string, any> | undefined} schema
 * @param {Record<string, any>} document
 * @param {number} [depth]
 * @returns {unknown}
 */
export function exampleFor(schema, document, depth = 0) {
    if (!schema || typeof schema !== 'object') return null;
    if (schema.$ref) {
        // The appendix expands a model reached at depth MAX_SCHEMA_DEPTH and no deeper.
        if (depth > MAX_SCHEMA_DEPTH) return {};
        return exampleFor(
            document.components?.schemas?.[refName(schema.$ref)],
            document,
            depth + 1,
        );
    }
    if (schema.example !== undefined) return schema.example;
    if (Array.isArray(schema.examples) && schema.examples.length > 0)
        return schema.examples[0];
    if (schema.default !== undefined) return schema.default;
    if (Array.isArray(schema.enum) && schema.enum.length > 0)
        return schema.enum[0];
    const nullable = nullableBranch(schema);
    if (nullable) return exampleFor(nullable, document, depth);

    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    const type =
        types.find((entry) => entry && entry !== 'null') ??
        (schema.properties ? 'object' : undefined);
    switch (type) {
        case 'object': {
            const value = {};
            for (const [name, property] of Object.entries(
                schema.properties ?? {},
            )) {
                value[name] = exampleFor(property, document, depth);
            }
            if (
                Object.keys(value).length === 0 &&
                typeof schema.additionalProperties === 'object'
            ) {
                value.key = exampleFor(
                    schema.additionalProperties,
                    document,
                    depth,
                );
            }
            return value;
        }
        case 'array':
            return [exampleFor(schema.items ?? {}, document, depth)];
        case 'integer':
        case 'number':
            return typeof schema.minimum === 'number' && schema.minimum > 0
                ? schema.minimum
                : 0;
        case 'boolean':
            return true;
        case 'string':
            return stringExample(schema);
        default:
            return null;
    }
}

/**
 * @param {Record<string, any>} schema A `type: string` schema.
 * @returns {string}
 */
function stringExample(schema) {
    switch (schema.format) {
        case 'uuid':
            return '01890a5d-ac96-774b-bcce-b302099a8057';
        case 'date-time':
            return '2026-01-01T00:00:00Z';
        case 'date':
            return '2026-01-01';
        case 'uri':
            return `${EXAMPLE_ORIGIN}/`;
        case 'byte':
            return 'AAECAw==';
        case 'binary':
            return '<bytes>';
        default:
            // The protocol header's grammar is a date; a placeholder that fails the pattern
            // would teach a request the server refuses.
            return schema.pattern === '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'
                ? '2026-01-01'
                : 'string';
    }
}

/**
 * The text an example request sends for a parameter: the document's example when it has
 * one, then the schema's; a string with no more to go on becomes the parameter's name in
 * capitals (`UPLOAD_ID`), which reads as "put yours here" where `string` would read as a
 * literal.
 *
 * @param {{ name: string, schema?: Record<string, any>, example?: unknown }} parameter
 * @param {Record<string, any>} document
 * @returns {string}
 */
function parameterValue(parameter, document) {
    const value = parameter.example ?? exampleFor(parameter.schema, document);
    if (value === 'string' || value === null || value === undefined) {
        return parameter.name.toUpperCase().replace(/-/g, '_');
    }
    return typeof value === 'object' ? JSON.stringify(value) : String(value);
}

/**
 * A value as one shell word, single-quoted.
 *
 * @param {string} value
 * @returns {string}
 */
function shellQuote(value) {
    return `'${value.replace(/'/g, `'\\''`)}'`;
}

/**
 * The `curl` request the panel shows: the method, the URL with every required query
 * parameter, the bearer header when the operation needs one, every required header, and a
 * body of the documented shape.
 *
 * @param {{ path: string, method: string, operation: Record<string, any> }} entry
 * @param {Record<string, any>} document
 * @returns {string}
 */
export function curlExample({ path, method, operation }, document) {
    const parameters = operation.parameters ?? [];
    const pathValue = (name) =>
        parameterValue(
            parameters.find((parameter) => parameter.name === name) ?? {
                name,
            },
            document,
        );
    // Every `{name}` filled in, so curl neither globs the braces nor sends them.
    const filled = path.replace(/\{([^}]+)\}/g, (_, name) =>
        encodeURIComponent(pathValue(name)),
    );
    const query = parameters
        .filter((parameter) => parameter.in === 'query' && parameter.required)
        .map(
            (parameter) =>
                `${parameter.name}=${encodeURIComponent(parameterValue(parameter, document))}`,
        );
    const url = `${EXAMPLE_ORIGIN}${filled}${query.length > 0 ? `?${query.join('&')}` : ''}`;
    // `-X HEAD` would wait for the body Content-Length announces; `-I` does not.
    const verb = method === 'HEAD' ? '-I' : `-X ${method}`;
    const lines = [`curl ${verb} ${shellQuote(url)}`];

    if (Array.isArray(operation.security) && operation.security.length > 0) {
        // Double-quoted so the shell expands the caller's $TOKEN.
        lines.push('-H "Authorization: Bearer $TOKEN"');
    }
    for (const parameter of [...parameters]
        .filter((entry) => entry.in === 'header' && entry.required)
        .sort((a, b) => a.name.localeCompare(b.name))) {
        lines.push(
            `-H ${shellQuote(`${parameter.name}: ${parameterValue(parameter, document)}`)}`,
        );
    }

    const body = bodyOf(operation.requestBody);
    if (body) {
        lines.push(`-H ${shellQuote(`Content-Type: ${body.mediaType}`)}`);
        if (body.mediaType.endsWith('json')) {
            lines.push(
                `-d ${shellQuote(JSON.stringify(exampleFor(body.schema, document), null, 2))}`,
            );
        } else {
            lines.push('--data-binary @body.bin');
        }
    }
    return lines.join(' \\\n  ');
}

/**
 * A code block for the panel: a fence long enough that nothing inside can close it.
 *
 * @param {string} language
 * @param {string} title
 * @param {string} code
 * @returns {string}
 */
function panelBlock(language, title, code) {
    const longest = Math.max(
        2,
        ...[...code.matchAll(/`+/g)].map((match) => match[0].length),
    );
    const fence = '`'.repeat(longest + 1);
    return `${fence}${language} title=${JSON.stringify(title)}\n${code}\n${fence}`;
}

/**
 * The right-hand panel: the request, then an example body for each documented success.
 *
 * @param {{ path: string, method: string, operation: Record<string, any> }} entry
 * @param {Record<string, any>} document
 * @returns {string}
 */
function renderPanel(entry, document) {
    const blocks = [panelBlock('sh', 'Request', curlExample(entry, document))];
    const successes = Object.entries(entry.operation.responses ?? {})
        .filter(([status]) => /^2/.test(status))
        .sort(([a], [b]) => a.localeCompare(b));
    for (const [status, response] of successes) {
        const body = bodyOf(response);
        if (!body) {
            blocks.push(
                `<p class="api-panel-note"><code>${status}</code> · no body</p>`,
            );
        } else if (body.mediaType.endsWith('json')) {
            blocks.push(
                panelBlock(
                    'json',
                    `${status} · ${body.mediaType}`,
                    JSON.stringify(exampleFor(body.schema, document), null, 2),
                ),
            );
        } else {
            blocks.push(
                `<p class="api-panel-note"><code>${status}</code> · <code>${body.mediaType}</code> bytes</p>`,
            );
        }
    }
    return blocks.join('\n\n');
}

/**
 * A response status as a pill coloured by class (`2xx`, `4xx`, …).
 *
 * @param {string} status
 * @returns {string}
 */
function statusPill(status) {
    return `<span class="api-status" data-class="${status.charAt(0)}xx">${status}</span>`;
}

/**
 * A labelled list of fields, the way the operation column shows parameters and responses:
 * each entry a name, its muted facts (where it goes, its type), a required marker, and its
 * description beneath. A list rather than a table, because the column is narrow and a
 * four-column table there wraps every header name across three lines.
 *
 * @param {string} label
 * @param {Array<{ name: string, meta: string[], required: boolean, description: string }>} fields
 * @returns {string}
 */
function fieldList(label, fields) {
    const items = fields.map(({ name, meta, required, description }) => {
        const head = [
            name,
            ...meta.map((fact) => `<span class="api-meta">${fact}</span>`),
            required ? '<span class="api-required">required</span>' : '',
        ]
            .filter(Boolean)
            .join(' ');
        return description
            ? `- ${head}\n\n  ${inlineProse(description)}`
            : `- ${head}`;
    });
    return [
        `<div class="api-fields" data-label="${label}">`,
        `**${label}**`,
        // Items separated by blank lines make a loose list, so every name and description
        // is a paragraph the stylesheet can reach, whether or not any entry has a description.
        items.join('\n\n'),
        '</div>',
    ].join('\n\n');
}

/**
 * Prose for a list-item paragraph: a `<` outside a code span is escaped, since Markdown would
 * read `<nonce>` as a tag and drop it, and a leading `#`, `>` or `1.` is escaped so it stays
 * text rather than opening a heading, quote or list inside the item.
 *
 * @param {string} text Single-line prose.
 * @returns {string}
 */
function inlineProse(text) {
    return text
        .split(/(`+[^`]*`+)/)
        .map((part, index) => (index % 2 ? part : part.replace(/</g, '&lt;')))
        .join('')
        .replace(/^(#|>|\d+\.)/, '\\$1');
}

/**
 * Render one operation: the reference on the left, the request and response examples on the
 * right. The wrapper is raw HTML around ordinary Markdown, so the heading, the tables and the
 * links stay Markdown that search, the table of contents and the link validator read.
 *
 * @param {{ path: string, method: string, operation: Record<string, any> }} entry
 * @param {string[]} documented Schema names the page's appendix carries.
 * @param {Record<string, any>} document
 * @returns {string}
 */
function renderOperation(entry, documented, document) {
    const { path, method, operation } = entry;
    const sections = [`## ${methodPill(method)} ${path}`];

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
            fieldList(
                'Parameters',
                [...parameters]
                    .sort(
                        (a, b) =>
                            a.in.localeCompare(b.in) ||
                            a.name.localeCompare(b.name),
                    )
                    .map((parameter) => ({
                        name: `\`${parameter.name}\``,
                        meta: [
                            parameter.in,
                            schemaLink(documented, parameter.schema ?? {}, {
                                inTable: false,
                            }),
                        ],
                        required: Boolean(parameter.required),
                        description: [
                            parameter.description
                                ? sentence(prose(parameter.description))
                                : '',
                            parameter.example === undefined
                                ? ''
                                : `Example: \`${parameter.example}\`.`,
                        ]
                            .filter(Boolean)
                            .join(' '),
                    })),
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
            fieldList(
                'Responses',
                responses.map(([status, response]) => {
                    const body = bodyOf(response);
                    const headers = Object.keys(response.headers ?? {}).sort();
                    return {
                        name: statusPill(status),
                        meta: body
                            ? [
                                  schemaLink(documented, body.schema, {
                                      inTable: false,
                                  }),
                                  `\`${body.mediaType}\``,
                              ]
                            : ['no body'],
                        required: false,
                        description: [
                            response.description
                                ? sentence(prose(response.description))
                                : '',
                            headers.length > 0
                                ? `Headers: ${headers.map((header) => `\`${header}\``).join(', ')}.`
                                : '',
                        ]
                            .filter(Boolean)
                            .join(' '),
                    };
                }),
            ),
        );
    }

    return [
        `<div class="api-op" data-method="${method.toLowerCase()}">`,
        '<div class="api-op-main">',
        '',
        sections.filter(Boolean).join('\n\n'),
        '',
        '</div>',
        '<div class="api-op-panel">',
        '',
        renderPanel(entry, document),
        '',
        '</div>',
        '</div>',
    ].join('\n');
}

/**
 * The page's index: one entry per operation, the method pill and the path, linking to the
 * operation, then the models the appendix documents. REST pages carry no table of contents,
 * so this is the page's navigation.
 *
 * @param {Array<{ path: string, method: string }>} operations
 * @param {string[]} documented Schema names the page's appendix carries.
 * @returns {string}
 */
function renderOperationIndex(operations, documented) {
    return [
        '<nav class="api-index" aria-label="On this page">',
        '',
        ...operations.map(
            ({ path, method }) =>
                `- [${methodPill(method)} \`${path}\`](#${operationAnchor(method, path)})`,
        ),
        ...(documented.length === 0
            ? []
            : [
                  '',
                  `**Schemas:** ${documented.map((name) => `[\`${name}\`](#${name.toLowerCase()})`).join(', ')}`,
              ]),
        '',
        '</nav>',
    ].join('\n');
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

    // Through `rewriteLinks` like every other prose site: a model's own doc comment is as
    // free to cite a design document by repo path, or an item by its rustdoc path, as a
    // handler's is. `TokenResponse` is one of several that do.
    if (schema.description) {
        sections.push(demoteHeadings(rewriteLinks(schema.description), 3));
    }

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
                escapeCell(
                    [
                        required.has(field) ? '**Required.**' : '',
                        property.description
                            ? sentence(prose(property.description))
                            : '',
                    ]
                        .filter(Boolean)
                        .join(' '),
                ) || '—',
            ]),
        ),
    );
    return sections.join('\n\n');
}

/**
 * The generated `/reference/api/conventions/` page: the document's `info.description`, the
 * contract every operation shares, which `capsule-server/src/openapi/contract.md` defines.
 *
 * @param {Record<string, any>} document
 * @returns {string} Markdown, frontmatter included.
 * @throws {Error} When the document has no description: the page would be empty, and every
 *   endpoint page sends its reader there.
 */
export function renderConventionsPage(document) {
    const description = document.info?.description;
    if (typeof description !== 'string' || description.trim() === '') {
        throw new Error(
            `${OPENAPI_DOCUMENT} has no info.description, which is the contract every ` +
                'operation shares (capsule-server/src/openapi/contract.md). Regenerate it with ' +
                '`mise run openapi-kynos`.',
        );
    }
    return `${tidyBlankLines(
        [
            '---',
            `title: ${yamlString(API_CONVENTIONS.label)}`,
            `description: ${yamlString(API_CONVENTIONS.description)}`,
            ...generatedFrontmatter(
                'REST API',
                `OpenAPI ${document.openapi}`,
                OPENAPI_DOWNLOAD,
            ),
            '---',
            '',
            `<!-- Generated from ${OPENAPI_DOCUMENT} by ${GENERATED_BY}. Do not edit. -->`,
            '',
            demoteHeadings(rewriteLinks(description.trim()), 0),
        ].join('\n'),
    )}\n`;
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
        'tableOfContents: false',
        ...generatedFrontmatter(
            'REST API',
            `OpenAPI ${document.openapi}`,
            OPENAPI_DOWNLOAD,
        ),
        '---',
        '',
        `<!-- Generated from ${OPENAPI_DOCUMENT} by ${GENERATED_BY}. Do not edit. -->`,
        '',
        'What every endpoint shares is on [](/reference/api/conventions/).',
        '',
        renderOperationIndex(operations, documented),
    ].join('\n');

    const body = operations
        .map((entry) => renderOperation(entry, documented, document))
        .join('\n\n');

    const appendix =
        documented.length === 0
            ? ''
            : [
                  '## Schemas',
                  '',
                  `The models these endpoints carry, to ${MAX_SCHEMA_DEPTH} references deep.`,
                  '',
                  documented
                      .map((name) => renderSchema(name, document, documented))
                      .join('\n\n'),
              ].join('\n');

    return `${tidyBlankLines([head, body, appendix].filter(Boolean).join('\n\n'))}\n`;
}

/**
 * One declaration on an SDK page: a function, a type, or a member of a type.
 *
 * Both SDK artifacts arrive in this shape — the binding dump carries it directly, and the wasm
 * digest is read into it by [`parseWasmSurface`] — so one renderer serves every SDK page.
 *
 * @typedef {object} SurfaceDeclaration
 * @property {string} kind What the declaration is, in its own language's words (`func`,
 *   `class`, `struct`, `data class`, `function`, …).
 * @property {string} name The name a caller writes.
 * @property {string} signature The declaration as the generated source spells it.
 * @property {string} doc Its doc comment, as Markdown, or `''`.
 * @property {SurfaceDeclaration[]} members A type's members, in source order; `[]` otherwise.
 */

/**
 * The text of a `/** … *\/` block opening at `lines[start]`, and the index after it.
 *
 * @param {string[]} lines
 * @param {number} start
 * @returns {[string, number]}
 */
function readJsDoc(lines, start) {
    const single = /^\s*\/\*\*(.*)\*\/\s*$/.exec(lines[start]);
    if (single) return [single[1].trim(), start + 1];
    const body = [];
    for (let i = start + 1; i < lines.length; i += 1) {
        if (/^\s*\*\/\s*$/.test(lines[i])) {
            return [body.join('\n').trim(), i + 1];
        }
        body.push(lines[i].replace(/^\s*\* ?/, ''));
    }
    throw new Error(
        `${WASM_SURFACE}: the doc comment opening on line ${start + 1} never closes`,
    );
}

/**
 * Read the committed wasm digest into declarations.
 *
 * The digest is TypeScript `wasm-bindgen` wrote, so this reads exactly the layout it writes and
 * refuses everything else: a top-level line it does not recognize is fatal rather than
 * skipped, because skipping it would publish a page missing a declaration and looking whole.
 * The layout is `wasm-bindgen`'s, pinned in `mise.toml`, so a bump that changes it fails here
 * and gets this reader updated.
 *
 * `private constructor();` is dropped: it is how `wasm-bindgen` says a class has no public
 * constructor, which the page says in words instead.
 *
 * @param {string} text The digest.
 * @returns {SurfaceDeclaration[]}
 */
export function parseWasmSurface(text) {
    const lines = text.split('\n');
    /** @type {SurfaceDeclaration[]} */
    const declarations = [];
    let doc = '';
    let i = 0;
    while (i < lines.length) {
        const line = lines[i];
        if (line.trim() === '' || line.startsWith('//')) {
            i += 1;
            continue;
        }
        if (line.startsWith('/**')) {
            [doc, i] = readJsDoc(lines, i);
            continue;
        }
        const fn = /^export function ([\w$]+)\(.*;$/.exec(line);
        if (fn) {
            declarations.push({
                kind: 'function',
                name: fn[1],
                signature: line,
                doc,
                members: [],
            });
            doc = '';
            i += 1;
            continue;
        }
        const type = /^export (class|enum|interface) ([\w$]+)\b.*\{$/.exec(
            line,
        );
        if (type) {
            const declaration = {
                kind: type[1],
                name: type[2],
                signature: line.replace(/\s*\{$/, ''),
                doc,
                members: [],
            };
            doc = '';
            i += 1;
            let memberDoc = '';
            while (i < lines.length && lines[i] !== '}') {
                const member = lines[i];
                if (member.trim() === '') {
                    i += 1;
                } else if (/^\s+\/\*\*/.test(member)) {
                    [memberDoc, i] = readJsDoc(lines, i);
                } else if (member.trim() === 'private constructor();') {
                    memberDoc = '';
                    i += 1;
                } else {
                    const name =
                        /^\s+(?:readonly\s+|static\s+)*([\w$]+|\[[^\]]+\])/.exec(
                            member,
                        );
                    if (!name) {
                        throw new Error(
                            `${WASM_SURFACE}:${i + 1}: cannot read a member of ` +
                                `${declaration.name} from "${member.trim()}"`,
                        );
                    }
                    declaration.members.push({
                        kind: /^\s+readonly\s/.test(member)
                            ? 'property'
                            : 'method',
                        name: name[1],
                        signature: member.trim(),
                        doc: memberDoc,
                        members: [],
                    });
                    memberDoc = '';
                    i += 1;
                }
            }
            if (i >= lines.length) {
                throw new Error(
                    `${WASM_SURFACE}: ${declaration.name} opens a block that never closes`,
                );
            }
            declarations.push(declaration);
            i += 1;
            continue;
        }
        throw new Error(
            `${WASM_SURFACE}:${i + 1}: unrecognized declaration "${line}". This reader knows ` +
                'the layout wasm-bindgen writes; update it rather than publishing a page that ' +
                'silently leaves the declaration out.',
        );
    }
    if (declarations.length === 0) {
        throw new Error(
            `${WASM_SURFACE} declares nothing. capsule-wasm exports a share viewer and a drop ` +
                'sealer; an empty digest is a regression in the digest step.',
        );
    }
    return declarations;
}

/**
 * The committed wasm surface, as declarations.
 *
 * @param {string} root Repository root.
 * @returns {SurfaceDeclaration[]}
 */
export function readWasmSurface(root) {
    return parseWasmSurface(readArtifactText(root, WASM_SURFACE));
}

/**
 * The committed uniffi binding dump, validated.
 *
 * @param {string} root Repository root.
 * @returns {Record<string, any>}
 */
export function readBindingsSurface(root) {
    const dump = readArtifact(root, BINDINGS_SURFACE);
    if (dump?.schema !== BINDINGS_SCHEMA) {
        throw new Error(
            `${BINDINGS_SURFACE} declares schema ${dump?.schema}, and this generator was ` +
                `written against schema ${BINDINGS_SCHEMA}. Update ${GENERATED_BY} rather than ` +
                'rendering a document it does not understand.',
        );
    }
    for (const language of ['swift', 'kotlin']) {
        const declarations = dump.languages?.[language]?.declarations;
        if (!Array.isArray(declarations) || declarations.length === 0) {
            throw new Error(
                `${BINDINGS_SURFACE} describes no ${language} declarations. Both languages ` +
                    'are generated from one cdylib, so an empty one is a regression in the ' +
                    'dump step, not a binding that shrank.',
            );
        }
    }
    return dump;
}

/**
 * Prose from a generated binding's doc comment, ready for a page body.
 *
 * The comments are Rust doc comments carried through a binding generator, so they keep
 * rustdoc's link forms. The `[text](path)` form is handled by [`rewriteLinks`]. The shorthand
 * ``[`item`]``, which rustdoc resolves by name, links to the item's entry when this page
 * documents it, and is unwrapped to its code span otherwise, because Markdown would print its
 * brackets.
 *
 * @param {string} doc
 * @param {number} offset Heading levels to demote by.
 * @param {ReadonlyMap<string, string>} [anchors] Symbol name to in-page anchor.
 * @returns {string}
 */
export function docProse(doc, offset, anchors = new Map()) {
    return demoteHeadings(docLinks(rewriteLinks(doc), anchors), offset);
}

/**
 * rustdoc's ``[`item`]`` shorthand, resolved: a link to the item's entry when the page
 * documents it, its bare code span otherwise.
 *
 * @param {string} text
 * @param {ReadonlyMap<string, string>} anchors
 * @returns {string}
 */
function docLinks(text, anchors) {
    return text.replace(/\[`([^`\]]+)`\](?![([])/g, (_, name) =>
        anchors.has(name)
            ? `[\`${name}\`](#${anchors.get(name)})`
            : `\`${name}\``,
    );
}

/**
 * A doc comment flattened for a list item or a table cell, its shorthand links resolved.
 *
 * @param {string} doc
 * @param {ReadonlyMap<string, string>} anchors
 * @param {'list' | 'cell'} context
 * @returns {string}
 */
function oneLine(doc, anchors, context) {
    const flat = docLinks(prose(doc), anchors);
    return context === 'cell' ? escapeCell(flat) : inlineProse(flat);
}

/**
 * `text` as an inline code span. Swift keeps the backticks it quotes a keyword with
 * (``public var `class`: FfiAlertClass``), and a single-backtick span would end at the first
 * of them, so such text gets a double-backtick delimiter with the padding CommonMark strips.
 *
 * @param {string} text
 * @returns {string}
 */
function codeSpan(text) {
    return text.includes('`') ? `\`\` ${text} \`\`` : `\`${text}\``;
}

/**
 * Each SDK language's page layout, after the documentation tool its developers already read:
 * Apple's DocC for Swift, Dokka for Kotlin, TypeDoc for the browser module.
 *
 * - `sections`: the page's declaration groups, in order. A declaration kind missing from
 *   every group is fatal, because it would otherwise be rendered under no heading or not at
 *   all.
 * - `kinds`: the label each declaration kind is shown with, in that tool's words.
 * - `topics`: how a type's members are grouped, in that tool's order. A member kind no topic
 *   lists is fatal for the same reason.
 * - `callable`: member kinds a caller calls. DocC and TypeDoc give each its own heading;
 *   Dokka lists them in a table, as it does on a class page.
 */
export const SDK_STYLES = {
    swift: {
        tool: 'docc',
        fence: 'swift',
        sections: [
            ['Functions', ['func']],
            ['Classes', ['class']],
            ['Protocols', ['protocol']],
            ['Structures', ['struct']],
            ['Enumerations', ['enum']],
        ],
        kinds: {
            func: 'Function',
            class: 'Class',
            protocol: 'Protocol',
            struct: 'Structure',
            enum: 'Enumeration',
        },
        topics: [
            ['Initializers', ['init']],
            ['Type Methods', ['static func']],
            ['Instance Methods', ['func']],
            ['Instance Properties', ['field']],
            ['Enumeration Cases', ['case']],
        ],
        callable: new Set(['init', 'static func', 'func']),
    },
    kotlin: {
        tool: 'dokka',
        fence: 'kotlin',
        sections: [
            ['Functions', ['fun']],
            ['Classes', ['class']],
            ['Interfaces', ['interface']],
            ['Data classes', ['data class']],
            ['Enums and sealed classes', ['enum class', 'sealed class']],
        ],
        kinds: {
            fun: 'fun',
            class: 'class',
            interface: 'interface',
            'data class': 'data class',
            'enum class': 'enum',
            'sealed class': 'sealed',
        },
        topics: [
            ['Constructors', ['constructor']],
            ['Functions', ['fun']],
            ['Properties', ['field']],
            ['Entries', ['entry']],
            ['Inheritors', ['variant']],
        ],
        callable: new Set(),
    },
    ts: {
        tool: 'typedoc',
        fence: 'ts',
        sections: [
            ['Functions', ['function']],
            ['Classes', ['class']],
            ['Interfaces', ['interface']],
            ['Enumerations', ['enum']],
        ],
        kinds: {
            function: 'Function',
            class: 'Class',
            interface: 'Interface',
            enum: 'Enumeration',
        },
        topics: [
            ['Properties', ['property']],
            ['Methods', ['method']],
        ],
        callable: new Set(['method']),
    },
};

/**
 * The first sentence of a doc comment: the one-line abstract DocC and Dokka show in a symbol
 * list. Code spans are kept whole, so a `.` inside one does not end the sentence.
 *
 * @param {string} doc
 * @returns {string}
 */
export function abstractOf(doc) {
    const paragraph = doc
        .trim()
        .split(/\n\s*\n/)[0]
        .replace(/\s*\n\s*/g, ' ');
    let inCode = false;
    for (let i = 0; i < paragraph.length; i += 1) {
        if (paragraph[i] === '`') inCode = !inCode;
        if (
            !inCode &&
            paragraph[i] === '.' &&
            (i + 1 === paragraph.length || paragraph[i + 1] === ' ')
        ) {
            return paragraph.slice(0, i + 1);
        }
    }
    return paragraph;
}

/**
 * A TypeScript function or method signature, split the way TypeDoc shows it: each parameter
 * with its type and whether it is optional, and the return type. `null` for a signature that
 * is not a call (a property).
 *
 * @param {string} signature One line of the digest.
 * @returns {{ parameters: Array<{ name: string, optional: boolean, type: string }>, returns: string } | null}
 */
export function tsSignature(signature) {
    const open = signature.indexOf('(');
    if (open === -1) return null;
    let depth = 0;
    let close = -1;
    const cuts = [];
    for (let i = open; i < signature.length; i += 1) {
        const c = signature[i];
        if ('(<[{'.includes(c)) depth += 1;
        else if (')>]}'.includes(c)) {
            depth -= 1;
            if (depth === 0) {
                close = i;
                break;
            }
        } else if (c === ',' && depth === 1) cuts.push(i);
    }
    if (close === -1) return null;
    const inner = signature.slice(open + 1, close);
    const parameters = [];
    if (inner.trim() !== '') {
        let start = 0;
        for (const cut of [...cuts.map((c) => c - open - 1), inner.length]) {
            const part = inner.slice(start, cut).trim();
            start = cut + 1;
            const match = /^([\w$]+)(\??):\s*(.+)$/.exec(part);
            if (!match) return null;
            parameters.push({
                name: match[1],
                optional: match[2] === '?',
                type: match[3],
            });
        }
    }
    const returns = /^\s*:\s*(.+?);?\s*$/.exec(signature.slice(close + 1));
    return { parameters, returns: returns ? returns[1] : 'void' };
}

/**
 * TypeDoc's Parameters and Returns blocks for a call signature, or `''` when the signature is
 * not one.
 *
 * @param {string} signature
 * @returns {string}
 */
function tsCallParts(signature) {
    const parts = tsSignature(signature);
    if (parts === null) return '';
    const blocks = [];
    if (parts.parameters.length > 0) {
        blocks.push(
            '<p class="sdk-label">Parameters</p>',
            parts.parameters
                .map(
                    ({ name, optional, type }) =>
                        `- \`${name}${optional ? '?' : ''}\`: ${codeSpan(type)}`,
                )
                .join('\n'),
        );
    }
    blocks.push('<p class="sdk-label">Returns</p>', codeSpan(parts.returns));
    return blocks.join('\n\n');
}

/**
 * The kind label a declaration is shown with, as the small eyebrow or chip above its name.
 *
 * @param {object} style One of [`SDK_STYLES`].
 * @param {string} kind
 * @returns {string}
 */
function kindChip(style, kind) {
    return `<p class="sdk-kind" data-kind="${kind.replace(/\s+/g, '-')}">${style.kinds[kind] ?? kind}</p>`;
}

/**
 * A type's members, grouped by `style.topics`: callables as headings of their own (DocC,
 * TypeDoc), values as a list, and in Dokka every member as a table row.
 *
 * @param {SurfaceDeclaration} declaration
 * @param {object} style
 * @param {number} level Heading level of the declaration.
 * @param {ReadonlyMap<string, string>} anchors
 * @param {string} source The artifact, for error messages.
 * @returns {string}
 */
function renderMembers(declaration, style, level, anchors, source) {
    const known = new Set(style.topics.flatMap(([, kinds]) => kinds));
    for (const member of declaration.members) {
        if (!known.has(member.kind)) {
            throw new Error(
                `${source}: ${declaration.name}.${member.name} is a ${member.kind}, which no ` +
                    `topic of the ${style.fence} page lists. Add it to SDK_STYLES in ${GENERATED_BY}.`,
            );
        }
    }
    const blocks = [];
    for (const [topic, kinds] of style.topics) {
        const members = declaration.members.filter((m) =>
            kinds.includes(m.kind),
        );
        if (members.length === 0) continue;
        blocks.push(`<p class="sdk-label">${topic}</p>`);
        if (
            style.tool !== 'dokka' &&
            kinds.some((kind) => style.callable.has(kind))
        ) {
            for (const member of members) {
                const sections = [
                    `${'#'.repeat(level + 1)} \`${member.name}\``,
                    `\`\`\`${style.fence} wrap\n${member.signature}\n\`\`\``,
                ];
                if (member.doc) {
                    sections.push(docProse(member.doc, level + 1, anchors));
                }
                if (style.tool === 'typedoc') {
                    const call = tsCallParts(member.signature);
                    if (call) sections.push(call);
                }
                blocks.push(sections.join('\n\n'));
            }
            continue;
        }
        // Values (and, in Dokka, every member): one row each. Dokka sets the name beside
        // the signature and its full KDoc; DocC and TypeDoc list the declaration and its
        // description. The description keeps its paragraphs and lists, nested in the item.
        const items = members.map((member) => {
            const lines =
                style.tool === 'dokka'
                    ? [
                          `- ${codeSpan(member.name)}`,
                          `\`\`\`${style.fence} wrap\n${member.signature}\n\`\`\``,
                      ]
                    : [`- ${codeSpan(member.signature)}`];
            if (member.doc) {
                lines.push(docProse(member.doc, level + 1, anchors));
            }
            return [lines[0], ...lines.slice(1).map(indentItem)].join('\n\n');
        });
        blocks.push(
            `<div class="sdk-members" data-tool="${style.tool}">\n\n${items.join('\n\n')}\n\n</div>`,
        );
    }
    return blocks.join('\n\n');
}

/**
 * `block` indented to continue a list item: every non-blank line by two spaces, so its
 * paragraphs, lists and fences stay inside the item.
 *
 * @param {string} block
 * @returns {string}
 */
function indentItem(block) {
    return block
        .split('\n')
        .map((line) => (line.trim() === '' ? '' : `  ${line}`))
        .join('\n');
}

/**
 * One top-level declaration in its language's layout.
 *
 * - DocC: kind eyebrow, name, abstract, Declaration, Overview, then Topics.
 * - Dokka: kind chip, name, signature, description, then member tables.
 * - TypeDoc: kind chip, name, signature, description, Parameters and Returns, members, and
 *   where it is defined.
 *
 * @param {SurfaceDeclaration} declaration
 * @param {object} style
 * @param {ReadonlyMap<string, string>} anchors
 * @param {string} source
 * @returns {string}
 */
function renderSymbol(declaration, style, anchors, source) {
    const level = 3;
    const signature = `\`\`\`${style.fence} wrap\n${declaration.signature}\n\`\`\``;
    const sections = [
        `<section class="sdk-symbol" data-tool="${style.tool}">`,
        kindChip(style, declaration.kind),
        `${'#'.repeat(level)} \`${declaration.name}\``,
    ];
    if (style.tool === 'docc') {
        const abstract = declaration.doc ? abstractOf(declaration.doc) : '';
        const overview = declaration.doc
            ? declaration.doc.trim().slice(abstract.length).trim()
            : '';
        if (abstract) {
            sections.push(
                `<div class="sdk-abstract">\n\n${docProse(abstract, level, anchors)}\n\n</div>`,
            );
        }
        sections.push('<p class="sdk-label">Declaration</p>', signature);
        if (overview) {
            sections.push(
                '<p class="sdk-label">Overview</p>',
                docProse(overview, level, anchors),
            );
        }
    } else {
        sections.push(signature);
        if (declaration.doc) {
            sections.push(docProse(declaration.doc, level, anchors));
        }
        if (style.tool === 'typedoc' && declaration.kind === 'function') {
            const call = tsCallParts(declaration.signature);
            if (call) sections.push(call);
        }
    }
    const members = renderMembers(declaration, style, level, anchors, source);
    if (members) {
        if (style.tool === 'docc') {
            sections.push('<p class="sdk-label sdk-topics">Topics</p>');
        }
        sections.push(members);
    }
    if (style.tool === 'typedoc') {
        sections.push(
            `<p class="sdk-source">Defined in <code>${source.split('/').pop()}</code></p>`,
        );
    }
    sections.push('</section>');
    return sections.join('\n\n');
}

/**
 * The page's symbol index, in the tool's own form: DocC's Topics (name and abstract), Dokka's
 * package summary table, TypeDoc's Index of names grouped by kind.
 *
 * @param {Array<[string, SurfaceDeclaration[]]>} groups
 * @param {object} style
 * @param {ReadonlyMap<string, string>} anchors
 * @returns {string}
 */
function renderSymbolIndex(groups, style, anchors) {
    const link = (d) => `[\`${d.name}\`](#${anchors.get(d.name)})`;
    const body = groups.map(([heading, declarations]) => {
        const label = `<p class="sdk-label">${heading}</p>`;
        if (style.tool === 'dokka') {
            return [
                label,
                table(
                    ['Name', 'Summary'],
                    declarations.map((d) => [
                        link(d),
                        d.doc
                            ? oneLine(abstractOf(d.doc), anchors, 'cell')
                            : '—',
                    ]),
                ),
            ].join('\n\n');
        }
        if (style.tool === 'docc') {
            return [
                label,
                declarations
                    .map((d) =>
                        d.doc
                            ? `- ${link(d)}\n\n  ${oneLine(abstractOf(d.doc), anchors, 'list')}`
                            : `- ${link(d)}`,
                    )
                    .join('\n\n'),
            ].join('\n\n');
        }
        return [label, declarations.map((d) => `- ${link(d)}`).join('\n')].join(
            '\n\n',
        );
    });
    return [
        `<nav class="sdk-index" data-tool="${style.tool}" aria-label="On this page">`,
        ...body,
        '</nav>',
    ].join('\n\n');
}

/**
 * The body of an SDK page: the symbol index, then one section per declaration group, in
 * [`SDK_STYLES`] order.
 *
 * @param {SurfaceDeclaration[]} declarations
 * @param {'swift' | 'kotlin' | 'ts'} language
 * @param {string} source The artifact, for error messages.
 * @returns {string}
 */
export function renderDeclarations(declarations, language, source) {
    const style = SDK_STYLES[language];
    const known = new Set(style.sections.flatMap(([, kinds]) => kinds));
    for (const declaration of declarations) {
        if (!known.has(declaration.kind)) {
            throw new Error(
                `${source}: ${declaration.name} is a ${declaration.kind}, which no section of ` +
                    `the ${language} page lists. Add the kind to SDK_STYLES in ${GENERATED_BY}.`,
            );
        }
    }
    const anchors = new Map(declarations.map((d) => [d.name, slugify(d.name)]));
    const groups = style.sections
        .map(([heading, kinds]) => [
            heading,
            declarations.filter((d) => kinds.includes(d.kind)),
        ])
        .filter(([, matching]) => matching.length > 0);
    return [
        renderSymbolIndex(groups, style, anchors),
        ...groups.map(([heading, matching]) =>
            [
                `## ${heading}`,
                ...matching.map((d) => renderSymbol(d, style, anchors, source)),
            ].join('\n\n'),
        ),
    ].join('\n\n');
}

/**
 * Frontmatter and banner for an SDK page.
 *
 * @param {import('./reference-groups.mjs').SdkPage} page
 * @param {string} artifact
 * @param {string[]} intro Prose lines after the banner.
 * @returns {string}
 */
function sdkHead(page, artifact, intro) {
    return [
        '---',
        `title: ${yamlString(page.label)}`,
        `description: ${yamlString(page.description)}`,
        ...generatedFrontmatter(`${page.label} SDK`, artifact),
        '---',
        '',
        `<!-- Generated from ${artifact} by ${GENERATED_BY}. Do not edit. -->`,
        '',
        ...intro,
    ].join('\n');
}

/**
 * The generated `/reference/sdk/wasm/` page.
 *
 * @param {import('./reference-groups.mjs').SdkPage} page
 * @param {SurfaceDeclaration[]} declarations
 * @returns {string} Markdown, frontmatter included.
 */
export function renderWasmPage(page, declarations) {
    const head = sdkHead(page, WASM_SURFACE, [
        ':::note[Usage]',
        'Classes have no public constructor: a function returns one, and the caller calls `free()`',
        'when done. Errors are thrown as an `Error` whose `message` is the stable code each',
        'description names.',
        ':::',
    ]);
    return `${tidyBlankLines(`${head}\n\n${renderDeclarations(declarations, 'ts', WASM_SURFACE)}`)}\n`;
}

/**
 * The generated `/reference/sdk/swift/` or `/reference/sdk/kotlin/` page.
 *
 * @param {import('./reference-groups.mjs').SdkPage} page
 * @param {Record<string, any>} dump The parsed binding dump.
 * @returns {string} Markdown, frontmatter included.
 */
export function renderBindingsPage(page, dump) {
    const language = page.language;
    const surface = dump.languages[language];
    const head = sdkHead(page, BINDINGS_SURFACE, [
        `Every declaration the \`${dump.namespace}\` namespace exports, as uniffi ${dump.uniffi} spells it in \`${surface.source}\`.`,
    ]);
    return `${tidyBlankLines(`${head}\n\n${renderDeclarations(surface.declarations, language, BINDINGS_SURFACE)}`)}\n`;
}

/**
 * Emit every generated reference page under `root`.
 *
 * Artifacts are read and validated first, before anything is written, so a missing or
 * unparseable one leaves no half-written section behind.
 *
 * @param {string} root Repository root.
 * @param {{ stale?: string[] }} [options] Artifacts that may be stale on this branch; their
 *   pages carry `generated.stale`, which the banner shows as a caution.
 * @returns {string[]} Repo-relative paths written, in a stable order.
 */
export function generate(root, { stale = [] } = {}) {
    const surface = readCliSurface(root);
    const document = readOpenApiDocument(root);
    const buckets = bucketOperations(document);
    const bindings = readBindingsSurface(root);
    const wasm = readWasmSurface(root);

    /** @type {Array<{ path: string, artifact: string, body: string }>} */
    const rendered = [
        {
            path: `${CLI_OUT}/${CLI_PAGES[0].slug}.md`,
            artifact: CLI_SURFACE,
            body: renderCliPage(surface),
        },
        {
            path: `${API_OUT}/${API_CONVENTIONS.slug}.md`,
            artifact: OPENAPI_DOCUMENT,
            body: renderConventionsPage(document),
        },
        ...API_GROUPS.map((group) => ({
            path: `${API_OUT}/${group.slug}.md`,
            artifact: OPENAPI_DOCUMENT,
            body: renderApiPage(group, buckets.get(group.slug) ?? [], document),
        })),
        ...SDK_PAGES.filter((page) => page.artifact !== null).map((page) => ({
            path: `${SDK_OUT}/${page.slug}.md`,
            artifact:
                page.artifact === 'wasm' ? WASM_SURFACE : BINDINGS_SURFACE,
            body:
                page.artifact === 'wasm'
                    ? renderWasmPage(page, wasm)
                    : renderBindingsPage(page, bindings),
        })),
    ];
    const pages = rendered.map(({ path, artifact, body }) => ({
        path,
        body: stale.includes(artifact)
            ? body.replace(/^generated:$/m, 'generated:\n  stale: true')
            : body,
    }));

    // Cleared, not merged into. A page this run no longer emits — a group renamed, a surface
    // dropped — would otherwise stay on disk, and because the directory is gitignored
    // `git status` never shows it: Astro would keep routing, indexing, and link-validating a
    // page no artifact describes, on this machine and on no CI runner. Only the generated
    // directories, so the hand-written overviews beside them survive.
    for (const directory of [CLI_OUT, API_OUT]) {
        rmSync(join(root, directory), { recursive: true, force: true });
    }
    // The SDK directory also holds the hand-written pages `SDK_PAGES` marks with a null
    // artifact, so every other file in it is cleared and those are kept.
    const handWritten = new Set(
        SDK_PAGES.filter((page) => page.artifact === null).map(
            (page) => `${page.slug}.md`,
        ),
    );
    if (existsSync(join(root, SDK_OUT))) {
        for (const entry of readdirSync(join(root, SDK_OUT))) {
            if (!handWritten.has(entry)) {
                rmSync(join(root, SDK_OUT, entry), {
                    recursive: true,
                    force: true,
                });
            }
        }
    }

    for (const { path, body } of pages) {
        mkdirSync(dirname(join(root, path)), { recursive: true });
        writeFileSync(join(root, path), body);
    }
    // Served beside the pages, so the REST banner's download link is the exact document the
    // pages were generated from. Gitignored like the pages: it is a copy, never a source.
    mkdirSync(join(root, PUBLIC_OUT), { recursive: true });
    writeFileSync(
        join(root, PUBLIC_OUT, OPENAPI_DOWNLOAD.slice(1)),
        readFileSync(join(root, OPENAPI_DOCUMENT)),
    );
    return pages.map(({ path }) => path);
}

/**
 * The artifacts that may be stale on this branch, each warned about on stderr with the
 * command that regenerates it. Empty, with the reason, where the check does not run.
 *
 * @param {string} root
 * @returns {string[]}
 */
function checkFreshness(root) {
    const skipped = skipReason(process.env);
    if (skipped) {
        process.stderr.write(
            `gen-reference: freshness check skipped in ${skipped}\n`,
        );
        return [];
    }
    const changes = readChanges(root);
    if (changes === null) {
        process.stderr.write(
            'gen-reference: freshness check skipped: git or origin/master is unavailable\n',
        );
        return [];
    }
    const stale = staleArtifacts(changes, ARTIFACT_SOURCES, mtimes(root));
    for (const artifact of stale) {
        process.stderr.write(
            `gen-reference: ${artifact} may be stale: its source changed on this branch after ` +
                `it was last regenerated. Run \`${ARTIFACT_SOURCES[artifact].regenerate}\`; the warning clears once it is rewritten.\n`,
        );
    }
    return stale;
}

function main() {
    // scripts/ -> capsule-docs/ -> repo root
    const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
    const written = generate(root, { stale: checkFreshness(root) });
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
