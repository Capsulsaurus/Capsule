// The site's navigation links: the header's section links and the footer's
// jump links. Their contents and order are a contract of the Visual Language
// doc (development/visual-language.md). Pure data and functions, so the
// header, the mobile menu and the footer render one tested list.
//
// On a locale's routes (src/lib/locales.mjs) every site link stays in that
// locale: Starlight serves each page at the locale's route, translated where a
// translation exists and in English with its fallback notice otherwise. The
// labels follow the locale too (src/lib/nav-labels.mjs).

import { localizePath } from './locales.mjs';
import { localizeLabel } from './nav-labels.mjs';

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

/**
 * Where the links are rendered: whether the blog is live (see
 * src/lib/blog-posts.mjs) and the locale of the page (a key of
 * src/lib/locales.mjs, undefined for English).
 *
 * @typedef {object} NavOptions
 * @property {boolean} blogIsLive
 * @property {string} [locale]
 */

/** @type {NavLink} */
const BLOG = { label: 'Blog', href: '/blog/', section: '/blog/' };

/**
 * A link as a locale renders it: its label translated and its site paths
 * under the locale's prefix. External links keep their address.
 *
 * @param {NavLink} link
 * @param {string | undefined} locale
 * @returns {NavLink}
 */
export function localizeLink(link, locale) {
    if (!locale) return link;
    return {
        ...link,
        label: localizeLabel(link.label, locale),
        ...(link.external ? {} : { href: localizePath(link.href, locale) }),
        ...(link.section
            ? { section: localizePath(link.section, locale) }
            : {}),
    };
}

/**
 * The header's section links, in order. Blog sits with the docs sections and
 * appears only once the blog is live.
 *
 * @param {NavOptions} options
 * @returns {NavLink[]}
 */
export function headerLinks({ blogIsLive, locale }) {
    return [
        {
            label: 'Guides',
            href: '/guides/getting-started/',
            section: '/guides/',
        },
        { label: 'Design', href: '/design/', section: '/design/' },
        { label: 'Reference', href: '/reference/', section: '/reference/' },
        ...(blogIsLive ? [BLOG] : []),
    ].map((link) => localizeLink(link, locale));
}

/**
 * The footer's jump links, in order.
 *
 * @param {NavOptions} options
 * @returns {NavLink[]}
 */
export function footerLinks({ blogIsLive, locale }) {
    return [
        { label: 'Documentation', href: '/guides/getting-started/' },
        ...(blogIsLive ? [{ label: BLOG.label, href: BLOG.href }] : []),
        {
            label: 'Roadmap',
            href: `${REPO}/blob/master/ROADMAP.md`,
            external: true,
        },
        { label: 'Discussions', href: `${REPO}/discussions`, external: true },
        { label: 'Support', href: '/support/' },
        { label: 'GitHub', href: REPO, external: true },
    ].map((link) => localizeLink(link, locale));
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

/**
 * The link whose section holds `pathname`, or `undefined` outside every
 * section (the front page, the support page). Names the narrow-screen section
 * menu's button.
 *
 * @param {NavLink[]} links
 * @param {string} pathname
 * @returns {NavLink | undefined}
 */
export function currentLink(links, pathname) {
    return links.find((link) => isActive(link, pathname));
}
