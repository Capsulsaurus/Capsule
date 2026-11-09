import { fromMarkdown } from 'mdast-util-from-markdown';
import { describe, expect, it } from 'vitest';
import remarkMermaid, {
    diagramFigure,
    HOUSE_CLASSES,
    renderDiagram,
    titleOf,
    withHouseClasses,
} from './remark-mermaid.mjs';

const FLOW = 'flowchart LR\n  a[Client]:::client --> b[(Store)]:::store';
const STATE =
    'stateDiagram-v2\n  [*] --> Active\n  Active --> Trashed: delete\n  Trashed --> [*]';

describe('titleOf', () => {
    it('reads a double- or single-quoted title from the fence meta', () => {
        expect(titleOf('title="Upload states"')).toBe('Upload states');
        expect(titleOf("title='Keys'")).toBe('Keys');
        expect(titleOf('')).toBeNull();
        expect(titleOf(undefined)).toBeNull();
    });
});

describe('withHouseClasses', () => {
    it('declares every house class right after a flowchart header', () => {
        const lines = withHouseClasses(FLOW).split('\n');
        expect(lines[0]).toBe('flowchart LR');
        for (const name of Object.keys(HOUSE_CLASSES)) {
            expect(lines).toContain(
                `  classDef ${name} ${HOUSE_CLASSES[name]}`,
            );
        }
    });

    it('leaves other diagram types alone', () => {
        expect(withHouseClasses(STATE)).toBe(STATE);
    });

    it('keeps every class value free of commas inside a value', () => {
        // classDef splits its style on commas; a color-mix() would break it.
        for (const style of Object.values(HOUSE_CLASSES)) {
            for (const part of style.split(',')) {
                expect(part).toMatch(/^[a-z-]+:var\(--[\w-]+\)$/);
            }
        }
    });
});

describe('renderDiagram', () => {
    it('renders a flowchart and a state diagram to SVG', () => {
        expect(renderDiagram(FLOW, 'd1', 'Flow')).toMatch(/^<svg id="d1"/);
        expect(renderDiagram(STATE, 'd2', 'States')).toMatch(/^<svg id="d2"/);
    });

    it('loads nothing remote and scopes its stylesheet to itself', () => {
        const svg = renderDiagram(FLOW, 'd1', 'Flow');
        expect(svg).not.toMatch(/https?:\/\/(?!www\.w3\.org)/);
        expect(svg).not.toContain('@import');
        expect(svg).toContain('#d1 {');
        expect(svg).toContain('#d1 text {');
        expect(svg).not.toMatch(/^\s*(svg|text) \{/m);
    });

    it('prefixes every id and reference, so diagrams on one page never collide', () => {
        const svg = renderDiagram(FLOW, 'd1', 'Flow');
        const ids = [...svg.matchAll(/\bid="([^"]+)"/g)].map((m) => m[1]);
        expect(ids.length).toBeGreaterThan(1);
        for (const id of ids) expect(id.startsWith('d1')).toBe(true);
        for (const [, ref] of svg.matchAll(/url\(#([^)]+)\)/g)) {
            expect(ids).toContain(ref);
        }
    });

    it('colours from page tokens and applies the house classes', () => {
        const svg = renderDiagram(FLOW, 'd1', 'Flow');
        expect(svg).toContain('var(--sl-color-white)');
        expect(svg).toContain('fill="var(--sl-color-accent-low)"');
        expect(svg).toContain('fill="var(--sl-color-green-low)"');
    });

    it('is deterministic', () => {
        expect(renderDiagram(FLOW, 'd1', 'Flow')).toBe(
            renderDiagram(FLOW, 'd1', 'Flow'),
        );
    });
});

describe('diagramFigure', () => {
    it('captions the figure and names the SVG, escaping the title', () => {
        const html = diagramFigure(FLOW, 'Client <-> "store"', 'd1');
        expect(html).toContain(
            '<figcaption>Client &lt;-&gt; &quot;store&quot;</figcaption>',
        );
        expect(html).toContain(
            'role="img" aria-label="Client &lt;-&gt; &quot;store&quot;"',
        );
    });
});

describe('remarkMermaid', () => {
    const run = (markdown) => {
        const tree = fromMarkdown(markdown);
        remarkMermaid()(tree, { path: 'page.md' });
        return tree;
    };

    it('replaces a mermaid fence with the figure and leaves other code alone', () => {
        const tree = run(
            `\`\`\`mermaid title="Flow"\n${FLOW}\n\`\`\`\n\n\`\`\`text\nplain\n\`\`\`\n`,
        );
        expect(tree.children[0].type).toBe('html');
        expect(tree.children[0].value).toMatch(/^<figure class="diagram/);
        expect(tree.children[1]).toMatchObject({ type: 'code', lang: 'text' });
    });

    it('gives two diagrams on a page distinct ids', () => {
        const tree = run(
            `\`\`\`mermaid title="One"\n${FLOW}\n\`\`\`\n\n\`\`\`mermaid title="Two"\n${FLOW}\n\`\`\`\n`,
        );
        const id = (node) => /<svg id="([^"]+)"/.exec(node.value)[1];
        expect(id(tree.children[0])).not.toBe(id(tree.children[1]));
    });

    it('refuses a diagram with no title, naming the page', () => {
        expect(() => run(`\`\`\`mermaid\n${FLOW}\n\`\`\`\n`)).toThrow(
            /page\.md: a mermaid diagram needs title=/,
        );
    });

    it('fails on a diagram that does not parse, naming it', () => {
        expect(() =>
            run('```mermaid title="Broken"\nnot-a-diagram ]]\n```\n'),
        ).toThrow(/page\.md: mermaid diagram "Broken" failed to render/);
    });
});
