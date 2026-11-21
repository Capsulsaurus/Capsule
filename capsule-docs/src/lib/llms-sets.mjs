/**
 * What `starlight-llms-txt` publishes for LLM agents, beyond Starlight's own config.
 *
 * `/llms.txt` is the entry point (https://llmstxt.org): a short orientation and links to
 * plain-text bundles of the docs, so an agent reads one file instead of crawling the HTML.
 * The plugin always emits `/llms-full.txt` (every published page) and `/llms-small.txt`
 * (note and tip asides and `<details>` stripped, `LLMS_SMALL_EXCLUDE` left out). The topic
 * sets below add `/_llms-txt/<label>.txt` bundles, rendered with the same stripping as
 * `/llms-small.txt`.
 *
 * Paths are micromatch globs over Starlight page ids (`guides/self-hosting`,
 * `design/cryptography` for a directory's `index.md`); in micromatch `x/**` also matches
 * `x`. Draft pages (`draft: true`) are never published. A glob that stops matching a
 * published page empties its bundle silently, so `llms-sets.test.mjs` proves every one
 * still matches. Pure data, no `node:` imports: `astro.config.mjs` imports it.
 */

/**
 * Orientation shown in `/llms.txt` after the site description. No headings: the format
 * reserves them for the link sections that follow. Each claim restates a doc it names.
 */
export const LLMS_DETAILS = `How to read these docs:

- The server stores only ciphertext and never decodes media; clients do all processing and encryption.
- A server needs PostgreSQL (its only database) and a blob directory on one filesystem behind a TLS-terminating reverse proxy (Guides → Self Hosting). The Self-Hosting set is the bundle to read for a deployment.
- The system invariants are in Design → Versioning: a version mismatch may refuse a write, never lose or corrupt data.
- Links inside the bundles are root-relative to this site.`;

/**
 * Topic bundles, each listed in `/llms.txt`.
 *
 * @type {Array<{ label: string, description: string, paths: string[] }>}
 */
export const LLMS_SETS = [
    {
        label: 'Self-Hosting',
        description:
            'deploying and operating capsule-server: requirements, configuration, storage layout, networking, upgrades, backup and recovery, troubleshooting, and the capsule CLI to verify a deployment',
        paths: [
            'guides/getting-started',
            'guides/self-hosting',
            'guides/troubleshooting',
            'design/networking',
            'design/filesystem/server',
            'design/filesystem/maintenance',
            'design/versioning',
            'design/backup-recovery',
            'reference/cli/**',
        ],
    },
    {
        label: 'Design',
        description:
            'the design docs: principles, cryptography, identity, storage, sync, sharing, federation and the threat model',
        paths: ['design/**'],
    },
    {
        label: 'API and SDKs',
        description:
            'the REST API and the Rust, Swift, Kotlin and browser SDK references, for building a client or integration',
        paths: ['reference/api/**', 'reference/sdk/**'],
    },
];

/**
 * Pages left out of `/llms-small.txt`; they remain in `/llms-full.txt`. The generated
 * reference is over half the site and has its own bundle; blog posts are not the contract.
 */
export const LLMS_SMALL_EXCLUDE = ['blog/**', 'reference/**'];

/** Pages sorted first in every bundle: the front page, then the guides. */
export const LLMS_PROMOTE = ['index*', 'guides/**'];
