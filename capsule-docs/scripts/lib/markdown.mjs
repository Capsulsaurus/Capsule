/**
 * Markdown helpers shared by the docs-truth checks.
 *
 * The anchor rules implemented here are GitHub's, not Starlight's, because the
 * links these checks validate are the ones read on GitHub: repo-relative `.md`
 * paths in `SLICES.md`, `AGENTS.md`, and the root docs. Links inside the
 * Starlight site are already proven by `starlight-links-validator` during
 * `mise run build-docs`, and are deliberately out of scope here.
 *
 * Three details of GitHub's slug are easy to get wrong and each one of them
 * costs real findings:
 *
 *   1. **Whitespace runs are preserved, never collapsed.** `## Damage Scenario →
 *      Invariant Map` slugs to `damage-scenario--invariant-map`, because the
 *      arrow is dropped and the two spaces around it each become a hyphen.
 *      Collapsing the run makes this checker reject 5 links `SLICES.md` gets
 *      right (`:21`, `:1895`, `:2142`, `:3643`, `:4616`), and would reject 36
 *      more inside the site if these checks reached there.
 *   2. **Underscores survive.** `### S-C21 — \`feed_seq\` visibility-order fix`
 *      keeps its underscore. Stripping `_` as an emphasis marker renames the
 *      four headings in this repository that carry an identifier (`SLICES.md`
 *      `:1909`, `:2290`, `:4853`, `:4891`), and no heading here actually uses
 *      `_emphasis_`.
 *   3. **Non-ASCII survives.** `\w` is ASCII-only, so an ASCII-only class drops
 *      `## 機能` (`README.ja.md`) to the empty string — losing the anchor
 *      entirely — and mangles the mixed Arabic headings in `README.ar.md`. 142
 *      headings across the translated READMEs are affected. The class below is
 *      Unicode-aware.
 */

/** Lines that open or close a fenced block, ``` or ~~~ with any info string. */
const FENCE = /^\s*(`{3,}|~{3,})/;

/** Blank every line of `text` while preserving how many lines it occupied. */
function blank(text) {
    return '\n'.repeat(text.split('\n').length - 1);
}

/**
 * Strip fenced code blocks, replacing each line with an empty one so that
 * 1-based line numbers computed downstream still point at the real source line.
 *
 * Leading whitespace is unbounded rather than CommonMark's three spaces,
 * because a fence nested in a list item is indented past that and is still a
 * fence — `capsule-cli/migration/README.md` has 36 of them.
 *
 * @param {string} source Markdown document.
 * @returns {string} The document with fenced content blanked out.
 */
export function stripFences(source) {
    let fence = null;
    return source
        .split('\n')
        .map((line) => {
            const match = FENCE.exec(line);
            if (fence === null) {
                if (!match) return line;
                fence = { char: match[1][0], length: match[1].length };
                return '';
            }
            // A closing fence uses the opener's character and is at least as
            // long, so neither a ``` inside a ~~~ block nor a ``` inside a ````
            // block ends it early.
            if (
                match &&
                match[1][0] === fence.char &&
                match[1].length >= fence.length
            ) {
                fence = null;
            }
            return '';
        })
        .join('\n');
}

/** Blank a leading YAML frontmatter block, whose values are data, not prose. */
export function stripFrontmatter(source) {
    const match = /^---\n[\s\S]*?\n---(\n|$)/.exec(source);
    return match ? blank(match[0]) + source.slice(match[0].length) : source;
}

/** Blank HTML comments — a link inside one is commented out, not published. */
export function stripComments(source) {
    return source.replace(/<!--[\s\S]*?-->/g, blank);
}

/** Everything above, in the order the document is layered. */
function prose(source) {
    return stripFences(stripComments(stripFrontmatter(source)));
}

/**
 * GitHub's heading-anchor slug: strip inline markup, lowercase, drop every
 * character outside letters, numbers, marks, `_`, `-`, and space, then map each
 * remaining space to a hyphen. Runs are preserved, never collapsed.
 *
 * @param {string} heading Raw heading text, without the leading `#`s.
 * @returns {string} The anchor slug, without a leading `#`.
 */
export function slugify(heading) {
    return heading
        .replace(/`/g, '')
        .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
        .replace(/\*/g, '')
        .replace(/\s*#+\s*$/, '')
        .trim()
        .toLowerCase()
        .replace(/[^\p{L}\p{N}\p{M}_\- ]/gu, '')
        .replace(/ /g, '-');
}

/**
 * Every anchor a Markdown document offers: one per ATX heading, plus any
 * explicit `<a id="...">`/`<a name="...">` a doc uses to pin a fragile slug.
 *
 * Repeated heading text gets GitHub's `-1`, `-2`, … disambiguating suffix.
 *
 * @param {string} source Markdown document.
 * @returns {Set<string>} Anchor slugs, without leading `#`.
 */
export function headingAnchors(source) {
    const anchors = new Set();
    const seen = new Map();
    // Explicit anchors are read before comments are stripped would remove them;
    // they live in real HTML, so only frontmatter and fences are taken out.
    const body = stripFences(stripFrontmatter(source));

    for (const line of body.split('\n')) {
        const heading = /^#{1,6}\s+(.*?)\s*$/.exec(line);
        if (!heading) continue;
        const base = slugify(heading[1]);
        if (!base) continue;
        const count = seen.get(base) ?? 0;
        seen.set(base, count + 1);
        anchors.add(count === 0 ? base : `${base}-${count}`);
    }

    for (const explicit of body.matchAll(
        /<a\s+(?:id|name)=["']([^"']+)["']/g,
    )) {
        anchors.add(explicit[1].toLowerCase());
    }

    return anchors;
}

/**
 * Every inline-link target in a document, with the 1-based line it sits on.
 * Reference-style definitions (`[label]: target`) count too — they are links
 * that happen to be declared away from their use.
 *
 * @param {string} source Markdown document.
 * @returns {{ target: string, line: number }[]}
 */
export function linkTargets(source) {
    const body = prose(source);
    const links = [];

    const push = (target, index) => {
        let trimmed = target.trim();
        // Angle-bracket form: `[a](<b c.md>)` quotes a target containing spaces.
        if (trimmed.startsWith('<') && trimmed.endsWith('>')) {
            trimmed = trimmed.slice(1, -1);
        }
        if (trimmed) {
            links.push({
                target: trimmed,
                line: body.slice(0, index).split('\n').length,
            });
        }
    };

    // Inline: [text](target) and ![alt](target). An escaped `\[` is not a link.
    // The target stops at the first whitespace so `[x](/a "title")` yields `/a`,
    // except in the angle-bracket form, which runs to its closing `>`.
    for (const match of body.matchAll(
        /(^|[^\\])(!?\[[^\]]*\]\(\s*)(<[^>]*>|[^)\s]+)/g,
    )) {
        // Offset past the guard character so the reported line is the link's.
        push(match[3], match.index + match[1].length);
    }
    // Reference definitions: [label]: target. A `^`-prefixed label is a
    // footnote definition, whose body is prose rather than a link target —
    // `design/ai.md` carries three, each opening with a word that would
    // otherwise be read as a relative path.
    for (const match of body.matchAll(/^\s{0,3}\[([^\]]+)\]:\s*(\S+)/gm)) {
        if (match[1].startsWith('^')) continue;
        push(match[2], match.index);
    }

    return links;
}
