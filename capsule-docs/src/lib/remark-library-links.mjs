// Remark plugin: links the first mention of each library or tool on a page to
// its source repository (src/lib/library-links.mjs), so a name never appears
// unlinked and no page carries a hand-copied URL. A mention is a whole-word
// prose spelling or an exact inline-code spelling; headings, existing links
// and code blocks are never touched, and later mentions stay plain text.
import { EXPLICIT_ONLY, LIBRARIES } from './library-links.mjs';

/** Node types whose text is never linked. */
const BLOCKED = new Set(['heading', 'link', 'linkReference', 'definition']);

/** @param {string} text */
function escapeRegExp(text) {
    return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * The matchers for a library list: one prose pattern over every name, longest
 * first so `TanStack Router` wins over a shorter overlap, and a map of exact
 * inline-code spellings.
 *
 * @param {import('./library-links.mjs').Library[]} libraries
 */
export function compile(libraries) {
    /** @type {Map<string, import('./library-links.mjs').Library>} */
    const byName = new Map();
    /** @type {Map<string, import('./library-links.mjs').Library>} */
    const byCode = new Map();
    for (const library of libraries) {
        for (const name of library.names) byName.set(name, library);
        for (const code of library.code ?? []) byCode.set(code, library);
    }
    const names = [...byName.keys()].sort((a, b) => b.length - a.length);
    // A name is a word on its own: not part of a path, an identifier, a
    // hyphenated compound or a file name.
    const prose =
        names.length === 0
            ? null
            : new RegExp(
                  `(?<![\\w./@-])(?:${names.map(escapeRegExp).join('|')})(?![\\w/@-]|\\.\\w)`,
                  'g',
              );
    return { byName, byCode, prose };
}

/**
 * Link the first mention of each library in an mdast tree, in place.
 *
 * @param {import('mdast').Root} tree
 * @param {import('./library-links.mjs').Library[]} [libraries]
 */
export function linkLibraries(tree, libraries = LIBRARIES) {
    const { byName, byCode, prose } = compile(libraries);
    /** @type {Set<import('./library-links.mjs').Library>} */
    const linked = new Set();

    const link = (library, child) => ({
        type: 'link',
        url: library.url,
        title: null,
        children: [child],
    });

    /** Split a text node around the first mention of each unlinked library. */
    const splitText = (value) => {
        if (!prose) return [{ type: 'text', value }];
        const out = [];
        let rest = 0;
        for (const match of value.matchAll(prose)) {
            const library = byName.get(match[0]);
            if (linked.has(library)) continue;
            linked.add(library);
            if (match.index > rest) {
                out.push({
                    type: 'text',
                    value: value.slice(rest, match.index),
                });
            }
            out.push(link(library, { type: 'text', value: match[0] }));
            rest = match.index + match[0].length;
        }
        if (rest < value.length) {
            out.push({ type: 'text', value: value.slice(rest) });
        }
        return out;
    };

    const walk = (node, blocked) => {
        if (!Array.isArray(node.children)) return;
        const inside = blocked || BLOCKED.has(node.type);
        const children = [];
        for (const child of node.children) {
            if (!inside && child.type === 'text') {
                children.push(...splitText(child.value));
            } else if (
                !inside &&
                child.type === 'inlineCode' &&
                byCode.has(child.value) &&
                !linked.has(byCode.get(child.value))
            ) {
                const library = byCode.get(child.value);
                linked.add(library);
                children.push(link(library, child));
            } else {
                walk(child, inside);
                children.push(child);
            }
        }
        node.children = children;
    };

    walk(tree, false);
    return tree;
}

/** Every name the map would match that EXPLICIT_ONLY forbids. */
export function forbiddenNames(libraries = LIBRARIES) {
    return libraries
        .flatMap((library) => [...library.names, ...(library.code ?? [])])
        .filter((name) => Object.hasOwn(EXPLICIT_ONLY, name));
}

export default function remarkLibraryLinks() {
    return (tree) => {
        linkLibraries(tree);
    };
}
