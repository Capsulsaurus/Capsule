import { rehype } from 'rehype';
import { describe, expect, it } from 'vitest';
import rehypeNoTranslate from './rehype-notranslate.mjs';

/** Run a fragment of HTML through the plugin and return the serialized result. */
async function run(html) {
    const file = await rehype()
        .data('settings', { fragment: true })
        .use(rehypeNoTranslate)
        .process(html);
    return String(file);
}

describe('rehypeNoTranslate', () => {
    it('marks inline <code> and fenced <pre> with translate="no"', async () => {
        const out = await run(
            '<p>See <code>crypto_suite_id</code>.</p><pre><code>let x = 1;</code></pre>',
        );
        expect(out).toContain('<code translate="no" class="notranslate">');
        expect(out).toContain('<pre translate="no" class="notranslate">');
    });

    it('wraps bare-prose primitive and brand names', async () => {
        const out = await run(
            '<p>Capsule hashes with SHA-256 and encrypts with AES-256-GCM.</p>',
        );
        expect(out).toContain(
            '<span translate="no" class="notranslate">Capsule</span>',
        );
        expect(out).toContain(
            '<span translate="no" class="notranslate">SHA-256</span>',
        );
        expect(out).toContain(
            '<span translate="no" class="notranslate">AES-256-GCM</span>',
        );
    });

    it('wraps terms inside table cells', async () => {
        const out = await run(
            '<table><tbody><tr><td>Bulk AEAD</td><td>AES-256-GCM</td></tr></tbody></table>',
        );
        expect(out).toContain(
            '<span translate="no" class="notranslate">AES-256-GCM</span>',
        );
    });

    it('does not wrap terms already inside <code>', async () => {
        const out = await run('<p>The <code>MLS</code> layer.</p>');
        // <code> itself is marked, but no inner span is added.
        expect(out).toContain(
            '<code translate="no" class="notranslate">MLS</code>',
        );
        expect(out).not.toContain('<span');
    });

    it('does not wrap or rewrite link text', async () => {
        const out = await run('<p><a href="/mls">MLS resilience</a></p>');
        expect(out).not.toContain('<span');
        expect(out).toContain('<a href="/mls">MLS resilience</a>');
    });

    it('prefers the longest matching term', async () => {
        const out = await run('<p>We use ML-KEM-768 for the KEM.</p>');
        expect(out).toContain(
            '<span translate="no" class="notranslate">ML-KEM-768</span>',
        );
        // The prefix term must not be split out on its own.
        expect(out).not.toContain('>ML-KEM</span>-768');
    });

    it('respects token boundaries (no match inside a longer word)', async () => {
        // "SHA-2" must not match inside "SHA-256"; "MLS" must not match inside
        // a hyphenated compound it does not own.
        const out = await run('<p>Algorithms: SHA-256 and a non-MLS path.</p>');
        expect(out).toContain(
            '<span translate="no" class="notranslate">SHA-256</span>',
        );
        expect(out).not.toContain('>SHA-2</span>');
        expect(out).not.toContain('non-<span');
    });

    it('leaves prose without any glossary term untouched', async () => {
        const out = await run('<p>This sentence has no protected terms.</p>');
        expect(out).toBe('<p>This sentence has no protected terms.</p>');
    });
});
