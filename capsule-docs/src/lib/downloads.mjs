// Every place Capsule is distributed from, as rendered by the downloads page
// (src/content/docs/downloads.mdx). Pure data, so the page renders one tested
// list (scripts/downloads.test.mjs) and a channel goes live by changing one
// entry here: set its `href` and flip its `status` to `available`.

import { REPO } from './site-nav.mjs';

/**
 * A download button. A `planned` channel is not out yet: it has no `href` and
 * renders as a "Coming soon" placeholder rather than a link.
 *
 * @typedef {object} Channel
 * @property {string} id
 * @property {string} label The button text.
 * @property {'available' | 'planned'} status
 * @property {string} [href] Absent exactly when `status` is `planned`.
 * @property {boolean} [external] True for an off-site `href`.
 * @property {import('@astrojs/starlight/types').StarlightIcon} [icon]
 */

/**
 * A platform the release workflow (.github/workflows/release.yml) builds an
 * archive for. Every archive holds the `capsule` command line; the workflow's
 * Unix packaging step also adds `capsule-server`, its Windows step does not.
 * A best-effort target is built with `optional: true`, so a release can ship
 * without it.
 *
 * @typedef {object} ReleaseTarget
 * @property {string} target The Rust target triple in the asset name.
 * @property {string} label
 * @property {Channel['icon']} icon
 * @property {boolean} includesServer
 * @property {boolean} bestEffort
 */

/** @type {ReleaseTarget[]} */
export const RELEASE_TARGETS = [
    {
        target: 'x86_64-unknown-linux-gnu',
        label: 'Linux (x86-64)',
        icon: 'linux',
        includesServer: true,
        bestEffort: false,
    },
    {
        target: 'aarch64-apple-darwin',
        label: 'macOS (Apple silicon)',
        icon: 'apple',
        includesServer: true,
        bestEffort: false,
    },
    {
        target: 'x86_64-apple-darwin',
        label: 'macOS (Intel)',
        icon: 'apple',
        includesServer: true,
        bestEffort: false,
    },
    {
        target: 'x86_64-pc-windows-msvc',
        label: 'Windows (x86-64)',
        icon: 'laptop',
        includesServer: false,
        bestEffort: true,
    },
];

/** @type {Channel[]} */
export const APPS = [
    {
        id: 'google-play',
        label: 'Google Play',
        status: 'planned',
        icon: 'phone',
        // TODO(track:deferred): set href to
        // https://play.google.com/store/apps/details?id=com.justin13888.capsule
        // and status to 'available' once the app is listed — see #389.
    },
    {
        id: 'app-store',
        label: 'App Store',
        status: 'planned',
        icon: 'apple',
        // TODO(track:deferred): set href to the app's apps.apple.com URL and
        // status to 'available' once it is listed; the URL needs the numeric
        // App Store id, which exists only after the listing — see #391–#393.
    },
];

/** @type {Channel[]} */
export const RELEASES = RELEASE_TARGETS.map((target) => ({
    id: `release-${target.target}`,
    label: target.includesServer
        ? target.label
        : `${target.label}, command line only`,
    status: 'available',
    // TODO: deep-link each archive
    // (releases/latest/download/capsule-v{version}-{target}.tar.gz, or .zip
    // on Windows) once the first release is published. The asset name carries
    // the version, so it cannot be linked before then.
    href: `${REPO}/releases/latest`,
    external: true,
    icon: target.icon,
}));

/** @type {Channel} */
export const ALL_RELEASES = {
    id: 'all-releases',
    label: 'All releases and notes',
    status: 'available',
    href: `${REPO}/releases`,
    external: true,
    icon: 'github',
};

/** @type {Channel} */
export const SERVER_IMAGE = {
    id: 'server-image',
    label: 'Container image',
    status: 'planned',
    icon: 'download',
    // TODO: publish a server container image (planned as
    // ghcr.io/capsulsaurus/capsule-server), then set href to its registry
    // page, status to 'available', and point SERVER_IMAGE_DOCS at the page
    // that documents running it.
};

/** @type {Channel} */
export const SERVER_IMAGE_DOCS = {
    id: 'server-image-docs',
    label: 'Self-hosting guide',
    status: 'available',
    // TODO: point at the container image's own docs page once it exists.
    href: '/guides/self-hosting/',
    icon: 'document',
};

/** Every channel, in the order the page shows them. */
export const CHANNELS = [
    ...APPS,
    ...RELEASES,
    ALL_RELEASES,
    SERVER_IMAGE,
    SERVER_IMAGE_DOCS,
];
