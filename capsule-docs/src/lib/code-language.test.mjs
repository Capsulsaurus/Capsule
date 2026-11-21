import { describe, expect, it } from 'vitest';
import {
    addLanguageLabel,
    LABEL_CLASS,
    LANGUAGE_NAMES,
    languageLabel,
    pluginLanguageLabel,
    UNLABELLED,
} from './code-language.mjs';

// A block as Expressive Code renders it: a wrapper, the frame, a header,
// the code and the copy button.
function block(frameClass = ['frame']) {
    return {
        type: 'element',
        tagName: 'div',
        properties: { className: ['expressive-code'] },
        children: [
            {
                type: 'element',
                tagName: 'figure',
                properties: { className: frameClass },
                children: [
                    {
                        type: 'element',
                        tagName: 'figcaption',
                        properties: { className: ['header'] },
                        children: [],
                    },
                    {
                        type: 'element',
                        tagName: 'pre',
                        properties: {},
                        children: [],
                    },
                    {
                        type: 'element',
                        tagName: 'div',
                        properties: { className: ['copy'] },
                        children: [],
                    },
                ],
            },
        ],
    };
}

const labels = (ast) =>
    ast.children[0].children.filter((n) =>
        n.properties?.className?.includes(LABEL_CLASS),
    );

describe('languageLabel', () => {
    it.each([
        ['kotlin', 'Kotlin'],
        ['swift', 'Swift'],
        ['sh', 'Shell'],
        ['bash', 'Bash'],
        ['json', 'JSON'],
        ['ts', 'TypeScript'],
        ['nginx', 'nginx'],
        ['rust', 'Rust'],
        ['yaml', 'YAML'],
    ])('names %s as %s', (id, name) => {
        expect(languageLabel(id)).toBe(name);
    });

    it('names every fence language the docs use', () => {
        // The ids in the docs' fences today; text and mermaid are unlabelled.
        for (const id of [
            'kotlin',
            'swift',
            'sh',
            'json',
            'ts',
            'nginx',
            'bash',
            'rust',
            'yaml',
        ]) {
            expect(LANGUAGE_NAMES[id], id).toBeTruthy();
        }
    });

    it('ignores case and surrounding space', () => {
        expect(languageLabel(' Kotlin ')).toBe('Kotlin');
        expect(languageLabel('JSON')).toBe('JSON');
    });

    it.each([...UNLABELLED])('shows nothing for %j', (id) => {
        expect(languageLabel(id)).toBeNull();
    });

    it('shows nothing for a fence with no language', () => {
        expect(languageLabel(undefined)).toBeNull();
        expect(languageLabel(null)).toBeNull();
    });

    it('shows an unknown id as written', () => {
        expect(languageLabel('hcl')).toBe('hcl');
        expect(languageLabel('Zig')).toBe('Zig');
    });
});

describe('addLanguageLabel', () => {
    it('appends one decorative tag to the frame, after the copy button', () => {
        const ast = block();
        expect(addLanguageLabel(ast, 'kotlin')).toBe(true);
        const [tag] = labels(ast);
        expect(tag).toEqual({
            type: 'element',
            tagName: 'span',
            properties: { className: [LABEL_CLASS], ariaHidden: 'true' },
            children: [{ type: 'text', value: 'Kotlin' }],
        });
        expect(ast.children[0].children.at(-1)).toBe(tag);
    });

    it('finds the frame among other classes', () => {
        const ast = block(['frame', 'is-terminal', 'not-content']);
        expect(addLanguageLabel(ast, 'sh')).toBe(true);
        expect(labels(ast)[0].children[0].value).toBe('Shell');
    });

    it('reads a class list written as a string', () => {
        const ast = block('frame has-title');
        expect(addLanguageLabel(ast, 'swift')).toBe(true);
    });

    it('labels a block only once', () => {
        const ast = block();
        addLanguageLabel(ast, 'json');
        expect(addLanguageLabel(ast, 'json')).toBe(false);
        expect(labels(ast)).toHaveLength(1);
    });

    it('leaves an unlabelled block alone', () => {
        const ast = block();
        expect(addLanguageLabel(ast, 'text')).toBe(false);
        expect(labels(ast)).toHaveLength(0);
    });

    it('leaves a block with no frame alone', () => {
        const ast = block(['not-a-frame']);
        expect(addLanguageLabel(ast, 'kotlin')).toBe(false);
        expect(labels(ast)).toHaveLength(0);
    });
});

describe('pluginLanguageLabel', () => {
    it('labels every block of a rendered group', () => {
        const plugin = pluginLanguageLabel();
        const kotlin = block();
        const text = block();
        const sh = block(['frame', 'is-terminal']);
        plugin.hooks.postprocessRenderedBlockGroup({
            renderedGroupContents: [
                { codeBlock: { language: 'kotlin' }, renderedBlockAst: kotlin },
                { codeBlock: { language: 'text' }, renderedBlockAst: text },
                { codeBlock: { language: 'sh' }, renderedBlockAst: sh },
            ],
        });
        expect(labels(kotlin)[0].children[0].value).toBe('Kotlin');
        expect(labels(text)).toHaveLength(0);
        expect(labels(sh)[0].children[0].value).toBe('Shell');
    });

    it('has a stable name', () => {
        expect(pluginLanguageLabel().name).toBe('capsule-language-label');
    });
});
