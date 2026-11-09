// Remark plugin: compiles ```mermaid fences to inline SVG at build time, with
// beautiful-mermaid, which lays diagrams out in pure JavaScript, so neither
// the build nor the reader needs a browser engine or a script. The SVG's
// colours are Starlight's CSS variables, so a diagram follows the theme
// toggle, and flowcharts may use the house classes below. The rules are a
// contract of the Visual Language doc (development/visual-language.md).
//
//   ```mermaid title="Upload session states"
//   flowchart LR
//     client[Client]:::client --> server[Server]:::server
//   ```
//
// The `title` is required: it is the figure's caption and accessible name.
import { createHash } from 'node:crypto';
import { renderMermaidSVG } from 'beautiful-mermaid';

/**
 * The house node classes, prepended to every flowchart. Values are Starlight
 * tokens, so both themes work unchanged.
 */
export const HOUSE_CLASSES = {
    client: 'fill:var(--sl-color-accent-low),stroke:var(--sl-color-accent),color:var(--sl-color-white)',
    server: 'fill:var(--sl-color-gray-6),stroke:var(--sl-color-gray-3),color:var(--sl-color-white)',
    store: 'fill:var(--sl-color-green-low),stroke:var(--sl-color-green),color:var(--sl-color-white)',
    external:
        'fill:var(--sl-color-bg),stroke:var(--sl-color-gray-4),color:var(--sl-color-gray-3)',
    danger: 'fill:var(--sl-color-red-low),stroke:var(--sl-color-red),color:var(--sl-color-white)',
    accent: 'fill:var(--sl-color-accent),stroke:var(--sl-color-accent-high),color:var(--sl-color-black)',
};

/** Diagram colours: page tokens, so the SVG follows the theme. */
const COLORS = {
    bg: 'var(--sl-color-bg)',
    fg: 'var(--sl-color-white)',
    line: 'var(--sl-color-gray-3)',
    accent: 'var(--sl-color-text-accent)',
    muted: 'var(--sl-color-gray-2)',
    surface: 'var(--sl-color-bg-nav)',
    border: 'var(--sl-color-gray-4)',
};

/**
 * The `title="…"` from a fence's meta string, or null.
 *
 * @param {string | null | undefined} meta
 * @returns {string | null}
 */
export function titleOf(meta) {
    const match = /\btitle=(?:"([^"]*)"|'([^']*)')/.exec(meta ?? '');
    return match ? (match[1] ?? match[2]) : null;
}

/** @param {string} text */
function escapeHtml(text) {
    return text
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

/**
 * The source with the house classes declared, for a flowchart; any other
 * diagram type is passed through (beautiful-mermaid styles only flowchart
 * nodes).
 *
 * @param {string} source
 * @returns {string}
 */
export function withHouseClasses(source) {
    const lines = source.split('\n');
    const header = lines.findIndex((line) => line.trim() !== '');
    if (!/^\s*(flowchart|graph)\b/.test(lines[header] ?? '')) return source;
    const classes = Object.entries(HOUSE_CLASSES).map(
        ([name, style]) => `  classDef ${name} ${style}`,
    );
    return [
        ...lines.slice(0, header + 1),
        ...classes,
        ...lines.slice(header + 1),
    ].join('\n');
}

/**
 * Render one diagram to self-contained inline SVG: no remote font import, its
 * stylesheet scoped to this SVG, and its element ids prefixed so several
 * diagrams on one page cannot collide.
 *
 * @param {string} source Mermaid source.
 * @param {string} id Unique id for the SVG element.
 * @param {string} label Accessible name, HTML-escaped.
 * @returns {string}
 */
export function renderDiagram(source, id, label) {
    const svg = renderMermaidSVG(withHouseClasses(source), {
        ...COLORS,
        font: 'Geist',
        // The figure card supplies the margin.
        padding: 16,
        transparent: true,
    });
    return (
        svg
            // The renderer imports its font from Google Fonts; the page already
            // serves Geist itself.
            .replace(/^\s*@import url\([^)]*\);\s*$/gm, '')
            // Inline SVG shares the page's stylesheet scope: confine its rules.
            .replace(/^(\s*)svg \{/m, `$1#${id} {`)
            .replace(/^(\s*)text \{/m, `$1#${id} text {`)
            .replace(/\bid="([^"]+)"/g, `id="${id}-$1"`)
            .replace(/url\(#([^)]+)\)/g, `url(#${id}-$1)`)
            .replace(/href="#([^"]+)"/g, `href="#${id}-$1"`)
            .replace(
                /^<svg /,
                `<svg id="${id}" role="img" aria-label="${label}" `,
            )
    );
}

/**
 * The figure that replaces a mermaid fence.
 *
 * @param {string} source
 * @param {string} title
 * @param {string} id
 * @returns {string}
 */
export function diagramFigure(source, title, id) {
    const caption = escapeHtml(title);
    return [
        '<figure class="diagram not-content">',
        renderDiagram(source, id, caption),
        `<figcaption>${caption}</figcaption>`,
        '</figure>',
    ].join('\n');
}

export default function remarkMermaid() {
    return (tree, file) => {
        const where = file.path ?? 'a docs page';
        const visit = (node) => {
            if (!Array.isArray(node.children)) return;
            node.children = node.children.map((child) => {
                if (child.type !== 'code' || child.lang !== 'mermaid') {
                    visit(child);
                    return child;
                }
                const title = titleOf(child.meta);
                if (!title) {
                    throw new Error(
                        `${where}: a mermaid diagram needs title="…", its caption and accessible name`,
                    );
                }
                const id = `diagram-${createHash('sha256')
                    .update(`${title}\n${child.value}`)
                    .digest('hex')
                    .slice(0, 10)}`;
                try {
                    return {
                        type: 'html',
                        value: diagramFigure(child.value, title, id),
                    };
                } catch (error) {
                    throw new Error(
                        `${where}: mermaid diagram "${title}" failed to render: ${error.message}`,
                    );
                }
            });
        };
        visit(tree);
    };
}
