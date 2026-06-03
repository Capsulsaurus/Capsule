/**
 * rehype plugin: opt technical content out of browser/machine auto-translation.
 *
 * Runs over the rendered HTML AST (hast) of every doc page and does two passes:
 *
 *   1. Code protection — marks every `<code>` and `<pre>` element with
 *      `translate="no"` (and `class="notranslate"` for Google Translate, which
 *      historically keys off the class rather than the attribute). This covers
 *      all backticked identifiers and fenced code blocks with zero source edits.
 *
 *   2. Glossary protection — wraps bare-prose occurrences of the terms in
 *      {@link file://./no-translate-terms.js} in `<span translate="no">`, so
 *      primitive/brand names that live in tables and sentences (not in backticks)
 *      survive translation intact.
 *
 * Text inside `<code>`/`<pre>`/`<a>` (and anything already marked `translate="no"`)
 * is left untouched, so the glossary never double-wraps or rewrites link text.
 *
 * @see https://html.spec.whatwg.org/multipage/dom.html#the-translate-attribute
 */

import { NO_TRANSLATE_TERMS } from './no-translate-terms.js';

/** Elements whose subtrees are skipped for glossary wrapping. */
const SKIP_SUBTREE = new Set(['code', 'pre', 'a', 'script', 'style']);

/** Escape a literal string for safe interpolation into a RegExp. */
function escapeRegExp(value) {
    return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * One case-sensitive pattern matching any glossary term as a standalone token.
 * Longest-first ordering ensures `ML-KEM-768` wins over `ML-KEM`; the `[\w-]`
 * lookarounds treat hyphens as part of a token so `SHA-2` never matches inside
 * `SHA-256` and `MLS` is not pulled out of `non-MLS`.
 */
const TERM_PATTERN = new RegExp(
    `(?<![\\w-])(?:${[...NO_TRANSLATE_TERMS]
        .sort((a, b) => b.length - a.length)
        .map(escapeRegExp)
        .join('|')})(?![\\w-])`,
    'g',
);

/** Add `translate="no"` and the `notranslate` class to a hast element in place. */
function markNoTranslate(node) {
    if (!node.properties) node.properties = {};
    const properties = node.properties;
    properties.translate = 'no';
    const existing = properties.className;
    if (Array.isArray(existing)) {
        if (!existing.includes('notranslate')) existing.push('notranslate');
    } else if (existing) {
        properties.className = [existing, 'notranslate'];
    } else {
        properties.className = ['notranslate'];
    }
}

/** Build a protected `<span>` hast element wrapping a matched term. */
function spanNode(value) {
    return {
        type: 'element',
        tagName: 'span',
        properties: { translate: 'no', className: ['notranslate'] },
        children: [{ type: 'text', value }],
    };
}

/**
 * Split a text value on glossary matches into a list of text/span hast nodes.
 * Returns `null` when there is nothing to wrap (so the caller can leave the
 * original node untouched).
 */
function splitOnTerms(value) {
    TERM_PATTERN.lastIndex = 0;
    const out = [];
    let last = 0;
    let match = TERM_PATTERN.exec(value);
    while (match !== null) {
        if (match.index > last) {
            out.push({ type: 'text', value: value.slice(last, match.index) });
        }
        out.push(spanNode(match[0]));
        last = match.index + match[0].length;
        match = TERM_PATTERN.exec(value);
    }
    if (out.length === 0) return null;
    if (last < value.length) {
        out.push({ type: 'text', value: value.slice(last) });
    }
    return out;
}

/** Recursively transform a hast node's children in place. */
function walk(node) {
    const children = node.children;
    if (!children) return;
    for (let i = 0; i < children.length; i++) {
        const child = children[i];
        if (child.type === 'element') {
            if (child.tagName === 'code' || child.tagName === 'pre') {
                markNoTranslate(child);
            }
            const alreadyMarked =
                child.properties && child.properties.translate === 'no';
            if (SKIP_SUBTREE.has(child.tagName) || alreadyMarked) continue;
            walk(child);
        } else if (child.type === 'text') {
            const replacement = splitOnTerms(child.value);
            if (replacement) {
                children.splice(i, 1, ...replacement);
                i += replacement.length - 1;
            }
        }
    }
}

/** rehype plugin factory. */
export default function rehypeNoTranslate() {
    return (tree) => {
        walk(tree);
        return tree;
    };
}
