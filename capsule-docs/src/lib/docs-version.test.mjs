import { describe, expect, it } from 'vitest';
import {
    addRelease,
    aliasHost,
    proxiedLocation,
    proxyTarget,
    releasedVersions,
    slashRedirect,
    stripBase,
    switchTarget,
    versionFromBranch,
    withBase,
} from './docs-version.mjs';

describe('versionFromBranch', () => {
    it('builds a release branch under /v/X.Y/ and anything else at /', () => {
        expect(versionFromBranch('docs/v0.1')).toEqual({
            version: '0.1',
            base: '/v/0.1/',
        });
        expect(versionFromBranch('master')).toEqual({
            version: null,
            base: '/',
        });
        expect(versionFromBranch(undefined)).toEqual({
            version: null,
            base: '/',
        });
        expect(versionFromBranch('docs/v0.1.2')).toEqual({
            version: null,
            base: '/',
        });
    });
});

describe('aliasHost', () => {
    it('names a branch deployment as Cloudflare does', () => {
        expect(aliasHost('docs/v0.1')).toBe('docs-v0-1.capsule-22k.pages.dev');
        expect(aliasHost('docs/v12.30', 'p')).toBe('docs-v12-30.p.pages.dev');
    });
});

describe('withBase', () => {
    const base = '/v/0.1/';

    it('prefixes a site-absolute link', () => {
        expect(withBase('/design/sync/', base)).toBe('/v/0.1/design/sync/');
        expect(withBase('/openapi.json', base)).toBe('/v/0.1/openapi.json');
    });

    it('leaves external, relative, in-page, shared and already-based links', () => {
        for (const href of [
            'https://example.com/',
            '//cdn.example/x',
            '#top',
            'sync/',
            '/rustdoc/capsule_sdk/',
            '/versions.json',
            '/v/0.2/design/',
            '/v/0.1/design/',
        ]) {
            expect(withBase(href, base)).toBe(href);
        }
    });

    it('is the identity on the latest build', () => {
        expect(withBase('/design/', '/')).toBe('/design/');
    });
});

describe('switching versions', () => {
    it('keeps the page across versions', () => {
        expect(stripBase('/v/0.1/design/sync/', '/v/0.1/')).toBe(
            '/design/sync/',
        );
        expect(switchTarget('/v/0.1/design/sync/', '/v/0.1/', '/')).toBe(
            '/design/sync/',
        );
        expect(switchTarget('/design/sync/', '/', '/v/0.2/')).toBe(
            '/v/0.2/design/sync/',
        );
        expect(switchTarget('/v/0.1/', '/v/0.1/', '/v/0.2/')).toBe('/v/0.2/');
    });
});

describe('proxyTarget', () => {
    it('sends an old version to its branch deployment', () => {
        expect(proxyTarget('/v/0.1/design/sync/')).toBe(
            'https://docs-v0-1.capsule-22k.pages.dev/design/sync/',
        );
        expect(proxyTarget('/v/0.1')).toBe(
            'https://docs-v0-1.capsule-22k.pages.dev/',
        );
    });

    it('sends rustdoc to GitHub Pages', () => {
        expect(proxyTarget('/rustdoc/capsule_sdk/index.html')).toBe(
            'https://capsulsaurus.github.io/Capsule/capsule_sdk/index.html',
        );
        expect(proxyTarget('/rustdoc')).toBe(
            'https://capsulsaurus.github.io/Capsule/',
        );
    });

    it('leaves every other path to the site', () => {
        expect(proxyTarget('/design/')).toBeNull();
        expect(proxyTarget('/v/latest/')).toBeNull();
        expect(proxyTarget('/rustdocs/')).toBeNull();
    });
});

describe('slashRedirect', () => {
    it('adds the slash a proxied root needs, and only there', () => {
        expect(slashRedirect('/rustdoc')).toBe('/rustdoc/');
        expect(slashRedirect('/v/0.1')).toBe('/v/0.1/');
        expect(slashRedirect('/rustdoc/')).toBeNull();
        expect(slashRedirect('/design')).toBeNull();
    });
});

describe('proxiedLocation', () => {
    it('keeps an upstream redirect on the docs domain', () => {
        expect(
            proxiedLocation(
                'https://capsulsaurus.github.io/Capsule/capsule_sdk/',
                '/rustdoc/capsule_sdk',
            ),
        ).toBe('/rustdoc/capsule_sdk/');
        expect(
            proxiedLocation(
                'https://docs-v0-1.capsule-22k.pages.dev/design/',
                '/v/0.1/design',
            ),
        ).toBe('/v/0.1/design/');
        expect(
            proxiedLocation('https://elsewhere.example/', '/v/0.1/x'),
        ).toBeNull();
    });
});

describe('versions.json', () => {
    it('lists valid versions newest first', () => {
        expect(
            releasedVersions({ versions: ['0.2', '0.10', 'x', 3, '1.0'] }),
        ).toEqual(['1.0', '0.10', '0.2']);
        expect(releasedVersions(null)).toEqual([]);
    });

    it('adds a release once, by its minor version', () => {
        expect(addRelease({ versions: [] }, '0.1.0')).toEqual({
            versions: ['0.1'],
        });
        expect(addRelease({ versions: ['0.1'] }, '0.1.3')).toEqual({
            versions: ['0.1'],
        });
        expect(addRelease({ versions: ['0.1'] }, '0.2.0')).toEqual({
            versions: ['0.2', '0.1'],
        });
        expect(() => addRelease({ versions: [] }, 'next')).toThrow(
            /not a release/,
        );
    });
});
