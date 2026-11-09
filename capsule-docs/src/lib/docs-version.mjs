// Which docs version a build is, and how its URLs are laid out. Shared by
// astro.config.mjs (the build's base path), the components that write
// site-absolute links, the version switcher, and the Pages Functions under
// functions/ that serve old versions and rustdoc on the docs domain.
//
// The layout (design/developer-docs.md, Hosting and Versions):
//   /            master, the production deployment ("Latest")
//   /v/X.Y/      release X.Y, its own deployment built from branch docs/vX.Y
//   /rustdoc/    the workspace rustdoc, served from GitHub Pages
//   /versions.json  the released versions, read at runtime by every version

/**
 * The `pages.dev` subdomain of the Cloudflare Pages project the docs deploy
 * to (project `capsule`; Cloudflare suffixed the subdomain because the plain
 * name was taken). A branch deployment is `<branch>.<this>.pages.dev`.
 */
export const PAGES_PROJECT = 'capsule-22k';

/** Where GitHub Pages serves the workspace rustdoc. */
export const RUSTDOC_ORIGIN = 'https://capsulsaurus.github.io/Capsule/';

/** Root paths every version shares, served by production, never prefixed. */
const SHARED_ROOTS = ['/rustdoc/', '/v/', '/versions.json'];

/**
 * The docs version a branch builds: `docs/vX.Y` is release X.Y, served under
 * `/v/X.Y/`; any other branch is the latest docs, served at `/`.
 *
 * @param {string | undefined} branch
 * @returns {{ version: string | null, base: string }}
 */
export function versionFromBranch(branch) {
    const match = /^docs\/v(\d+\.\d+)$/.exec(branch ?? '');
    return match
        ? { version: match[1], base: `/v/${match[1]}/` }
        : { version: null, base: '/' };
}

/**
 * The branch a release version is built from.
 *
 * @param {string} version `X.Y`
 * @returns {string}
 */
export function branchForVersion(version) {
    return `docs/v${version}`;
}

/**
 * The `pages.dev` host of a branch deployment, as Cloudflare names its
 * alias: lowercased, every run of other characters a hyphen.
 *
 * @param {string} branch
 * @param {string} [project]
 * @returns {string}
 */
export function aliasHost(branch, project = PAGES_PROJECT) {
    const alias = branch
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-|-$/g, '');
    return `${alias}.${project}.pages.dev`;
}

/**
 * A site-absolute link as this build must write it: under the build's base,
 * unless it is external, protocol-relative, in-page, relative, already under
 * the base, or one of the roots every version shares.
 *
 * @param {string} href
 * @param {string} base `/` or `/v/X.Y/`
 * @returns {string}
 */
export function withBase(href, base) {
    if (base === '/' || !href.startsWith('/') || href.startsWith('//')) {
        return href;
    }
    if (href.startsWith(base)) return href;
    if (SHARED_ROOTS.some((root) => href === root || href.startsWith(root))) {
        return href;
    }
    return `${base.slice(0, -1)}${href}`;
}

/**
 * The build's base, as `/` or `/x/y/`, from Astro's `import.meta.env.BASE_URL`.
 *
 * @param {string} baseUrl
 * @returns {string}
 */
export function normalizeBase(baseUrl) {
    return baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
}

/**
 * `pathname` without the build's base, as the site's own routes spell it.
 *
 * @param {string} pathname
 * @param {string} base
 * @returns {string}
 */
export function stripBase(pathname, base) {
    return base !== '/' && pathname.startsWith(base)
        ? `/${pathname.slice(base.length)}`
        : pathname;
}

/**
 * The same page in another version: the path under `fromBase`, moved under
 * `toBase`. The switcher falls back to `toBase` itself when the page does not
 * exist there.
 *
 * @param {string} pathname
 * @param {string} fromBase
 * @param {string} toBase
 * @returns {string}
 */
export function switchTarget(pathname, fromBase, toBase) {
    return withBase(stripBase(pathname, fromBase), toBase);
}

/**
 * Where a request to production is proxied: an old version to its branch
 * deployment, rustdoc to GitHub Pages. `null` for anything else.
 *
 * @param {string} pathname
 * @param {string} [project]
 * @returns {string | null}
 */
export function proxyTarget(pathname, project = PAGES_PROJECT) {
    const version = /^\/v\/(\d+\.\d+)(\/.*)?$/.exec(pathname);
    if (version) {
        const host = aliasHost(branchForVersion(version[1]), project);
        return `https://${host}${version[2] ?? '/'}`;
    }
    const rustdoc = /^\/rustdoc(\/.*)?$/.exec(pathname);
    if (rustdoc) {
        return `${RUSTDOC_ORIGIN}${(rustdoc[1] ?? '/').slice(1)}`;
    }
    return null;
}

/**
 * The slashed path for a proxied root requested without its slash (`/rustdoc`,
 * `/v/X.Y`), or `null`. Served without the slash, the page's relative links
 * would resolve against `/`.
 *
 * @param {string} pathname
 * @returns {string | null}
 */
export function slashRedirect(pathname) {
    return /^\/(rustdoc|v\/\d+\.\d+)$/.test(pathname) ? `${pathname}/` : null;
}

/**
 * A redirect from the proxied origin, rewritten back onto the docs domain, so
 * a reader is never sent off it. `null` when it points elsewhere.
 *
 * @param {string} location The upstream `Location` header, absolute.
 * @param {string} pathname The path the reader asked for.
 * @param {string} [project]
 * @returns {string | null}
 */
export function proxiedLocation(location, pathname, project = PAGES_PROJECT) {
    if (location.startsWith(RUSTDOC_ORIGIN)) {
        return `/rustdoc/${location.slice(RUSTDOC_ORIGIN.length)}`;
    }
    const version = /^\/v\/(\d+\.\d+)/.exec(pathname);
    if (version) {
        const origin = `https://${aliasHost(branchForVersion(version[1]), project)}/`;
        if (location.startsWith(origin)) {
            return `/v/${version[1]}/${location.slice(origin.length)}`;
        }
    }
    return null;
}

/**
 * The released versions in `/versions.json`, newest first, validated.
 *
 * @param {unknown} json
 * @returns {string[]}
 */
export function releasedVersions(json) {
    const versions = /** @type {any} */ (json)?.versions;
    if (!Array.isArray(versions)) return [];
    return versions
        .filter((v) => typeof v === 'string' && /^\d+\.\d+$/.test(v))
        .sort((a, b) => {
            const [am, an] = a.split('.').map(Number);
            const [bm, bn] = b.split('.').map(Number);
            return bm - am || bn - an;
        });
}

/**
 * `json` with release `X.Y.Z`'s minor version added once.
 *
 * @param {unknown} json
 * @param {string} release
 * @returns {{ versions: string[] }}
 */
export function addRelease(json, release) {
    const match = /^(\d+)\.(\d+)\.\d+/.exec(release);
    if (!match) throw new Error(`not a release version: ${release}`);
    const minor = `${match[1]}.${match[2]}`;
    const versions = releasedVersions(json);
    return {
        versions: releasedVersions({
            versions: versions.includes(minor)
                ? versions
                : [...versions, minor],
        }),
    };
}
