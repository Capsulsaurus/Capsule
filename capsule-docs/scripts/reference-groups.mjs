/**
 * The `/reference/` page table — the one place the reference section's shape is decided.
 *
 * `design/developer-docs.md` requires the `Reference` sidebar to be hand-curated, "in the
 * same style as `Design` and for the same reason: generated pages must not be allowed to
 * determine navigation order". Autogenerating it from the emitted directory would order
 * pages by filename, which is a fact about slugs rather than a decision about reading
 * order.
 *
 * Hand-curated does not have to mean written twice. Both `gen-reference.mjs` and
 * `astro.config.mjs` import this file: the generator buckets the description artifacts into
 * these pages, the config builds the sidebar from the same list. Editing the order here
 * moves the page and its navigation entry together, and a group that has navigation but no
 * page — or a page nothing links to — is not expressible.
 *
 * This module is deliberately data plus two pure functions, with no `node:` imports, so the
 * Astro config can import it in the browser-facing build without dragging filesystem code
 * along.
 */

/**
 * A generated page.
 *
 * @typedef {object} ReferencePage
 * @property {string} slug Last path segment of the route, and the emitted file's basename.
 * @property {string} label Sidebar label and page title.
 * @property {string} description Frontmatter description, shown in search results.
 */

/**
 * The CLI pages, in reading order.
 *
 * One page rather than one per command: `capsule` has 16 commands whose help is a sentence
 * each, and sixteen pages of one paragraph would put the whole surface behind sixteen
 * clicks. The command tree is small enough to read end to end.
 *
 * @type {ReferencePage[]}
 */
export const CLI_PAGES = [
    {
        slug: 'commands',
        label: 'Commands',
        description:
            'Every capsule command, argument, and option, generated from the committed command tree.',
    },
];

/**
 * Sidebar items for the `Reference` group, in the order they are read.
 *
 * Returns Starlight sidebar entries: the section overview first, then one nested group per
 * surface whose own overview leads its generated pages.
 *
 * @returns {Array<{ slug: string } | { label: string, items: Array<{ slug: string }> }>}
 */
export function referenceSidebar() {
    return [
        { slug: 'reference' },
        {
            label: 'CLI',
            items: [
                { slug: 'reference/cli' },
                ...CLI_PAGES.map((page) => ({
                    slug: `reference/cli/${page.slug}`,
                })),
            ],
        },
    ];
}
