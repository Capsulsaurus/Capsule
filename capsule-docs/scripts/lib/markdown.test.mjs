import { describe, expect, it } from 'vitest';
import {
    headingAnchors,
    linkTargets,
    slugify,
    stripComments,
    stripFences,
    stripFrontmatter,
} from './markdown.mjs';

describe('slugify', () => {
    it('preserves whitespace runs rather than collapsing them', () => {
        // The regression that matters: "→" is dropped as a non-word character
        // and the two spaces around it each become a hyphen. A slugger that
        // collapses the run reports every citation of this heading as broken.
        expect(slugify('Damage Scenario → Invariant Map')).toBe(
            'damage-scenario--invariant-map',
        );
        expect(slugify('Scan → Extract')).toBe('scan--extract');
        expect(slugify('Surface ↔ Transport Map')).toBe(
            'surface--transport-map',
        );
    });

    it('strips inline code, emphasis, and link syntax', () => {
        expect(slugify('The `tpm` feature')).toBe('the-tpm-feature');
        expect(slugify('**Write** Authorization')).toBe('write-authorization');
        expect(slugify('See [Keys](/design/cryptography/keys/)')).toBe(
            'see-keys',
        );
    });

    it('keeps underscores, which headings use as identifiers not emphasis', () => {
        // `SLICES.md:1909` and six siblings. Stripping `_` as an emphasis
        // marker silently renames every one of their anchors.
        expect(slugify('S-C21 — `feed_seq` visibility-order fix')).toBe(
            's-c21--feed_seq-visibility-order-fix',
        );
        expect(slugify('`device_id` on session listing')).toBe(
            'device_id-on-session-listing',
        );
    });

    it('keeps non-ASCII letters', () => {
        // `\w` is ASCII-only: an ASCII class slugs `## 機能` to the empty string
        // and drops the heading entirely, and mangles mixed Arabic headings.
        expect(slugify('機能')).toBe('機能');
        expect(slugify('نظرة عامة')).toBe('نظرة-عامة');
    });

    it('drops a closing hash run', () => {
        expect(slugify('Key Chain ##')).toBe('key-chain');
    });

    it('drops punctuation and lowercases', () => {
        expect(slugify('Deletes Are Soft First.')).toBe(
            'deletes-are-soft-first',
        );
        expect(slugify('Why?')).toBe('why');
        expect(slugify('Delegated/Sponsored accounts')).toBe(
            'delegatedsponsored-accounts',
        );
    });
});

describe('stripFences', () => {
    it('blanks fenced content while preserving line numbering', () => {
        const source = ['a', '```js', 'const x = 1;', '```', 'b'].join('\n');
        expect(stripFences(source).split('\n')).toEqual(['a', '', '', '', 'b']);
    });

    it('recognises a fence indented past four spaces inside a list item', () => {
        // `capsule-cli/migration/README.md` has 36 of these. A three-space
        // bound leaves their contents scanned as prose.
        const source = [
            '- step:',
            '',
            '    ```sh',
            '    [x](gone.md)',
            '    ```',
        ].join('\n');
        expect(linkTargets(source)).toEqual([]);
    });

    it('does not let a shorter fence close a longer one', () => {
        const source = [
            '````',
            '```',
            '[x](gone.md)',
            '````',
            '[y](kept.md)',
        ].join('\n');
        expect(linkTargets(source).map((l) => l.target)).toEqual(['kept.md']);
    });

    it('does not let a different fence character close a block early', () => {
        const source = ['~~~', '```', '[x](/nope)', '~~~', '[y](/yes)'].join(
            '\n',
        );
        const targets = linkTargets(stripFences(source)).map((l) => l.target);
        expect(targets).toEqual(['/yes']);
    });
});

describe('stripFrontmatter and stripComments', () => {
    it('blanks frontmatter without shifting line numbers', () => {
        const source = [
            '---',
            'title: T',
            'description: see [x](gone.md)',
            '---',
            '[y](kept.md)',
        ];
        const links = linkTargets(source.join('\n'));
        expect(links.map((l) => l.target)).toEqual(['kept.md']);
        expect(links[0].line).toBe(5);
    });

    it('blanks HTML comments, whose links are commented out', () => {
        expect(
            linkTargets('<!-- [x](gone.md) -->\n[y](kept.md)').map(
                (l) => l.target,
            ),
        ).toEqual(['kept.md']);
    });

    it('preserves line counts', () => {
        expect(
            stripFrontmatter('---\na: 1\n---\nbody').split('\n'),
        ).toHaveLength(4);
        expect(stripComments('a\n<!--\nx\n-->\nb').split('\n')).toHaveLength(5);
    });

    it('leaves an explicit anchor element visible to headingAnchors', () => {
        // Comments are stripped for links but not for anchors: `<a id=…>` is
        // real HTML a doc uses to pin a fragile slug.
        expect(headingAnchors('<a id="CHK-IDX-017"></a>\n')).toEqual(
            new Set(['chk-idx-017']),
        );
    });
});

describe('headingAnchors', () => {
    it('collects one anchor per heading', () => {
        const anchors = headingAnchors(
            '# Title\n\n## Key Chain\n\n### Device Directory\n',
        );
        expect(anchors).toEqual(
            new Set(['title', 'key-chain', 'device-directory']),
        );
    });

    it('disambiguates repeated headings the way GitHub does', () => {
        const anchors = headingAnchors(
            '## Failure Modes\n\n## Failure Modes\n\n## Failure Modes\n',
        );
        expect(anchors).toEqual(
            new Set(['failure-modes', 'failure-modes-1', 'failure-modes-2']),
        );
    });

    it('ignores headings inside fenced blocks', () => {
        expect(headingAnchors('```\n# Not A Heading\n```\n')).toEqual(
            new Set(),
        );
    });

    it('picks up explicit anchor elements', () => {
        expect(headingAnchors('<a id="CHK-IDX-017"></a>\n')).toEqual(
            new Set(['chk-idx-017']),
        );
    });
});

describe('linkTargets', () => {
    it('finds inline links and images', () => {
        const targets = linkTargets('[a](one.md) and ![b](two.png)').map(
            (l) => l.target,
        );
        expect(targets).toEqual(['one.md', 'two.png']);
    });

    it('stops the target at a title string', () => {
        expect(linkTargets('[a](one.md "A title")')[0].target).toBe('one.md');
    });

    it('finds reference definitions but not footnote definitions', () => {
        // `design/ai.md` carries three footnotes whose bodies open with a word
        // that a naive reference-definition pattern reads as a relative path.
        const source = [
            '[ref]: real.md',
            '[^note]: Considered and rejected: SigLIP.',
        ].join('\n');
        expect(linkTargets(source).map((l) => l.target)).toEqual(['real.md']);
    });

    it('unwraps the angle-bracket target form', () => {
        expect(linkTargets('[a](<b c.md>)')[0].target).toBe('b c.md');
    });

    it('ignores an escaped bracket, which is not a link', () => {
        expect(linkTargets('\\[a](b.md)')).toEqual([]);
    });

    it('reports the 1-based line of each link', () => {
        expect(linkTargets('x\n\n[a](one.md)')[0].line).toBe(3);
    });
});
