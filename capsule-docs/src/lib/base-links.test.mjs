import { describe, expect, it } from 'vitest';
import { rebaseHtml } from './base-links.mjs';

describe('rebaseHtml', () => {
    it('puts every site-absolute link under a release base, once', () => {
        const html = rebaseHtml(
            '<a href="/design/">d</a><img src="/x.png" srcset="/x.png 1x"><a href="https://e.x/">e</a><link href="/v/0.1/_astro/a.css" rel="stylesheet"><a href="/rustdoc/">r</a>',
            '/v/0.1/',
        );
        expect(html).toContain('<a href="/v/0.1/design/">d</a>');
        expect(html).toContain('src="/v/0.1/x.png"');
        expect(html).toContain('srcset="/v/0.1/x.png 1x"');
        expect(html).toContain('href="https://e.x/"');
        expect(html).toContain('href="/v/0.1/_astro/a.css"');
        expect(html).toContain('<a href="/rustdoc/">r</a>');
    });

    it('keeps a redirect page inside the version', () => {
        const html = rebaseHtml(
            '<meta http-equiv="refresh" content="0;url=/design/principles/">',
            '/v/0.1/',
        );
        expect(html).toContain('content="0;url=/v/0.1/design/principles/"');
    });

    it('returns the latest build untouched', () => {
        const html = '<a href="/design/">d</a>';
        expect(rebaseHtml(html, '/')).toBe(html);
    });
});
