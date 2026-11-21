// A small tag in the corner of every rendered code block naming its language
// ("Kotlin", "Shell", "JSON"), so a reader can tell at a glance what a
// snippet is written in. An Expressive Code plugin (`pluginLanguageLabel`,
// registered in astro.config.mjs) appends the tag to each block's frame once
// the frame is rendered; its look lives in src/styles/global.css (Code
// blocks). The naming is a pure function, tested here. The contract is
// development/visual-language.md (Docs Site, Code blocks).

/**
 * Display names by fence language id, lower-case. An id not listed here is
 * shown as written.
 *
 * @type {Readonly<Record<string, string>>}
 */
export const LANGUAGE_NAMES = Object.freeze({
    astro: 'Astro',
    bash: 'Bash',
    c: 'C',
    console: 'Console',
    cpp: 'C++',
    css: 'CSS',
    diff: 'Diff',
    dockerfile: 'Dockerfile',
    go: 'Go',
    graphql: 'GraphQL',
    html: 'HTML',
    http: 'HTTP',
    ini: 'INI',
    java: 'Java',
    javascript: 'JavaScript',
    js: 'JavaScript',
    json: 'JSON',
    json5: 'JSON5',
    jsonc: 'JSONC',
    jsx: 'JSX',
    kotlin: 'Kotlin',
    kt: 'Kotlin',
    kts: 'Kotlin',
    markdown: 'Markdown',
    md: 'Markdown',
    mdx: 'MDX',
    nginx: 'nginx',
    pkl: 'Pkl',
    powershell: 'PowerShell',
    proto: 'Protobuf',
    ps1: 'PowerShell',
    py: 'Python',
    python: 'Python',
    rs: 'Rust',
    rust: 'Rust',
    sh: 'Shell',
    shell: 'Shell',
    shellscript: 'Shell',
    sql: 'SQL',
    swift: 'Swift',
    toml: 'TOML',
    ts: 'TypeScript',
    tsx: 'TSX',
    typescript: 'TypeScript',
    xml: 'XML',
    yaml: 'YAML',
    yml: 'YAML',
    zsh: 'Zsh',
});

/**
 * Ids that name no language worth showing: plain text, terminal output, and
 * fences another plugin renders as something other than code.
 */
export const UNLABELLED = new Set([
    '',
    'ansi',
    'mermaid',
    'plain',
    'plaintext',
    'text',
    'txt',
]);

/** The class of the tag, styled in src/styles/global.css. */
export const LABEL_CLASS = 'capsule-code-language';

/**
 * The name to show for a fence's language id, or `null` for none.
 *
 * @param {string | null | undefined} id
 * @returns {string | null}
 */
export function languageLabel(id) {
    const key = (id ?? '').trim().toLowerCase();
    if (UNLABELLED.has(key)) return null;
    return LANGUAGE_NAMES[key] ?? (id ?? '').trim();
}

/**
 * Appends the tag to a rendered block's frame (`<figure class="frame">`).
 * Returns whether it did: a block with no frame, an unlabelled language or a
 * tag already in place is left alone.
 *
 * @param {import('hast').Element} block the block's rendered HAST
 * @param {string | null | undefined} language the fence's language id
 * @returns {boolean}
 */
export function addLanguageLabel(block, language) {
    const label = languageLabel(language);
    if (!label) return false;
    const frame = findFrame(block);
    if (!frame) return false;
    if (frame.children.some(isLabel)) return false;
    frame.children.push({
        type: 'element',
        tagName: 'span',
        // Decorative: the code's language is already in `data-language`, and a
        // screen reader reading the snippet does not need it announced.
        properties: { className: [LABEL_CLASS], ariaHidden: 'true' },
        children: [{ type: 'text', value: label }],
    });
    return true;
}

/**
 * @param {import('hast').Element} node
 * @returns {import('hast').Element | undefined}
 */
function findFrame(node) {
    if (node.tagName === 'figure' && classes(node).includes('frame')) {
        return node;
    }
    for (const child of node.children ?? []) {
        if (child.type !== 'element') continue;
        const frame = findFrame(child);
        if (frame) return frame;
    }
    return undefined;
}

/** @param {import('hast').ElementContent} node */
function isLabel(node) {
    return node.type === 'element' && classes(node).includes(LABEL_CLASS);
}

/** @param {import('hast').Element} node */
function classes(node) {
    const value = node.properties?.className;
    if (Array.isArray(value)) return value.map(String);
    return typeof value === 'string' ? value.split(/\s+/) : [];
}

/**
 * The Expressive Code plugin. It runs on each finished block group, after
 * every block's frame exists, whatever order the plugins were given in.
 *
 * @returns {{ name: string, hooks: { postprocessRenderedBlockGroup: (context: { renderedGroupContents: readonly { codeBlock: { language: string }, renderedBlockAst: import('hast').Element }[] }) => void } }}
 */
export function pluginLanguageLabel() {
    return {
        name: 'capsule-language-label',
        hooks: {
            postprocessRenderedBlockGroup({ renderedGroupContents }) {
                for (const {
                    codeBlock,
                    renderedBlockAst,
                } of renderedGroupContents) {
                    addLanguageLabel(renderedBlockAst, codeBlock.language);
                }
            },
        },
    };
}
