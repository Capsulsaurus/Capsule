// The downloads page (src/content/docs/downloads.mdx) renders every channel
// from src/lib/downloads.mjs. The link validator checks only the site's own
// pages, so this test keeps the rest honest: a channel that is not out yet
// renders no link, a live one points into the repository, the release targets
// match what the release workflow actually publishes, and every docs link
// resolves to an authored page.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
    ALL_RELEASES,
    APPS,
    CHANNELS,
    RELEASE_TARGETS,
    RELEASES,
    SERVER_IMAGE,
    SERVER_IMAGE_DOCS,
} from '../src/lib/downloads.mjs';
import { REPO } from '../src/lib/site-nav.mjs';
import { contentFileForRoute } from './lib/routes.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const DOCS = join(ROOT, 'capsule-docs/src/content/docs');
const PAGE = readFileSync(join(DOCS, 'downloads.mdx'), 'utf8');
const RELEASE_WORKFLOW = readFileSync(
    join(ROOT, '.github/workflows/release.yml'),
    'utf8',
);

/** True when a site-relative path is served by an authored docs page. */
const hasPage = (path) => contentFileForRoute(DOCS, path) !== null;

/** The release matrix's entries: `target`, `os` and `optional` per target. */
const MATRIX = [
    ...RELEASE_WORKFLOW.matchAll(
        /^\s+- target: (\S+)\n\s+os: (\S+)\n\s+optional: (true|false)$/gm,
    ),
].map(([, target, os, optional]) => ({
    target,
    os,
    optional: optional === 'true',
}));

/** The body of the release workflow's step named `name`. */
function step(name) {
    const start = RELEASE_WORKFLOW.indexOf(`- name: ${name}\n`);
    expect(start, name).toBeGreaterThan(-1);
    const next = RELEASE_WORKFLOW.indexOf('- name: ', start + 1);
    return RELEASE_WORKFLOW.slice(start, next === -1 ? undefined : next);
}

/** The channels downloads.mdx renders, by the export each one comes from. */
const EXPORTS = {
    APPS,
    RELEASES,
    ALL_RELEASES,
    SERVER_IMAGE,
    SERVER_IMAGE_DOCS,
};

describe('download channels', () => {
    it('lists every channel once', () => {
        const ids = CHANNELS.map((channel) => channel.id);
        expect(new Set(ids).size).toBe(ids.length);
        expect(CHANNELS).toEqual([
            ...APPS,
            ...RELEASES,
            ALL_RELEASES,
            SERVER_IMAGE,
            SERVER_IMAGE_DOCS,
        ]);
    });

    it.each(
        CHANNELS.filter((channel) => channel.status === 'planned').map(
            (channel) => [channel.id, channel],
        ),
    )('planned channel %s renders no link', (_, channel) => {
        expect(channel.href).toBeUndefined();
    });

    it.each(
        CHANNELS.filter((channel) => channel.status === 'available').map(
            (channel) => [channel.id, channel],
        ),
    )('available channel %s links somewhere real', (_, channel) => {
        expect(typeof channel.href).toBe('string');
        if (channel.href.startsWith('/')) {
            expect(hasPage(channel.href)).toBe(true);
            expect(Boolean(channel.external)).toBe(false);
        } else {
            expect(channel.href.startsWith(`${REPO}/`)).toBe(true);
            expect(channel.external).toBe(true);
        }
    });

    it('keeps the app stores and the server image as placeholders', () => {
        expect(APPS.map((channel) => channel.status)).toEqual([
            'planned',
            'planned',
        ]);
        expect(SERVER_IMAGE.status).toBe('planned');
    });
});

describe('GitHub releases', () => {
    it('offers exactly the targets the release workflow publishes', () => {
        expect(MATRIX.length).toBeGreaterThan(0);
        expect(RELEASE_TARGETS.map((target) => target.target).sort()).toEqual(
            MATRIX.map((entry) => entry.target).sort(),
        );
    });

    it.each(
        RELEASE_TARGETS.map((target) => [target.target, target]),
    )('%s says whether its archive holds the server, as the workflow packages it', (name, target) => {
        const entry = MATRIX.find((row) => row.target === name);
        const windows = entry.os.startsWith('windows');
        const unix = step('Package (unix)');
        const win = step('Package (windows)');
        expect(unix).toContain("if: runner.os != 'Windows'");
        expect(win).toContain("if: runner.os == 'Windows'");
        expect(target.includesServer).toBe(
            (windows ? win : unix).includes('release/capsule-server'),
        );
    });

    it.each(
        RELEASE_TARGETS.map((target) => [target.target, target]),
    )('%s is best-effort exactly when the workflow lets it fail', (name, target) => {
        const entry = MATRIX.find((row) => row.target === name);
        expect(target.bestEffort).toBe(entry.optional);
    });

    it('labels an archive without the server as command line only', () => {
        for (const [i, target] of RELEASE_TARGETS.entries()) {
            expect(RELEASES[i].label.endsWith(', command line only')).toBe(
                !target.includesServer,
            );
        }
    });

    it('tells the reader which archives are best-effort', () => {
        expect(RELEASE_TARGETS.some((target) => target.bestEffort)).toBe(true);
        expect(PAGE).toMatch(/best-effort/);
    });

    it('gives one button per target', () => {
        expect(RELEASES.map((channel) => channel.id)).toEqual(
            RELEASE_TARGETS.map((target) => `release-${target.target}`),
        );
    });
});

describe('downloads page', () => {
    it('renders every channel, through DownloadButton', () => {
        expect(PAGE).toContain("from '../../lib/downloads.mjs'");
        const rendered = Object.entries(EXPORTS).flatMap(([name, value]) => {
            const list = Array.isArray(value);
            const used = list
                ? PAGE.includes(
                      `{${name}.map((channel) => <DownloadButton channel={channel} />)}`,
                  )
                : PAGE.includes(`<DownloadButton channel={${name}}`);
            expect(used, name).toBe(true);
            return list ? value : [value];
        });
        expect(rendered.map((channel) => channel.id).sort()).toEqual(
            CHANNELS.map((channel) => channel.id).sort(),
        );
    });

    it('hard-codes no store or registry URL', () => {
        expect(PAGE).not.toMatch(/play\.google\.com|apps\.apple\.com|ghcr\.io/);
    });

    it('names the self-hosting guide as the server image docs for now', () => {
        expect(SERVER_IMAGE_DOCS.href).toBe('/guides/self-hosting/');
    });
});
