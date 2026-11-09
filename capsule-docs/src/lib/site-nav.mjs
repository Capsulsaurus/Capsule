// The site's navigation links: the header's section links and the footer's
// jump links. Their contents and order are a contract of the Visual Language
// doc (development/visual-language.md). Pure data and functions, so the
// header, the mobile menu and the footer render one tested list.

export const REPO = 'https://github.com/Capsulsaurus/Capsule';

/**
 * A navigation link. `section` is the path prefix that marks the link as the
 * current one; links without a section are never current.
 *
 * @typedef {object} NavLink
 * @property {string} label
 * @property {string} href
 * @property {string} [section]
 * @property {boolean} [external]
 */

/** @type {NavLink} */
const BLOG = { label: 'Blog', href: '/blog/', section: '/blog/' };

/**
 * The header's section links, in order. Blog sits with the docs sections and
 * appears only once the blog is live (see src/lib/blog-posts.mjs).
 *
 * @param {{ blogIsLive: boolean }} options
 * @returns {NavLink[]}
 */
export function headerLinks({ blogIsLive }) {
    return [
        {
            label: 'Guides',
            href: '/guides/getting-started/',
            section: '/guides/',
        },
        { label: 'Design', href: '/design/', section: '/design/' },
        { label: 'Reference', href: '/reference/', section: '/reference/' },
        ...(blogIsLive ? [BLOG] : []),
    ];
}

/**
 * The footer's jump links, in order.
 *
 * @param {{ blogIsLive: boolean }} options
 * @returns {NavLink[]}
 */
export function footerLinks({ blogIsLive }) {
    return [
        { label: 'Documentation', href: '/guides/getting-started/' },
        ...(blogIsLive ? [{ label: BLOG.label, href: BLOG.href }] : []),
        {
            label: 'Roadmap',
            href: `${REPO}/blob/master/ROADMAP.md`,
            external: true,
        },
        { label: 'Discussions', href: `${REPO}/discussions`, external: true },
        { label: 'GitHub', href: REPO, external: true },
    ];
}

/**
 * The link's `aria-current` value: `page` when it points at the current page,
 * `true` when the current page lies elsewhere in its section, otherwise
 * `undefined` (no attribute).
 *
 * @param {NavLink} link
 * @param {string} pathname
 * @returns {'page' | 'true' | undefined}
 */
export function ariaCurrent(link, pathname) {
    if (!isActive(link, pathname)) return undefined;
    return withSlash(pathname) === link.href ? 'page' : 'true';
}

/** @param {string} path */
function withSlash(path) {
    return path.endsWith('/') ? path : `${path}/`;
}

/**
 * True when `pathname` lies in the link's section. A section matches only at
 * a path-segment boundary, so `/design/` does not claim `/designer/`.
 *
 * @param {NavLink} link
 * @param {string} pathname
 * @returns {boolean}
 */
export function isActive(link, pathname) {
    if (!link.section) return false;
    return withSlash(pathname).startsWith(link.section);
}
