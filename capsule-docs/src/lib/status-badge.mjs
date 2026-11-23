// The single mapping from a page's computed audit status (see
// src/lib/page-audit.mjs) to the badge that shows it. The page title and the
// sidebar both read it, so the two can never disagree.

/** @typedef {import('./page-audit.mjs').AuditStatus} AuditStatus */
/** @typedef {{ text: string, variant: 'success' | 'caution' }} StatusBadge */

/** @type {Record<AuditStatus, StatusBadge>} */
const TITLE_BADGES = {
    audited: { text: 'Audited', variant: 'success' },
    outdated: { text: 'Changed since audit', variant: 'caution' },
    unaudited: { text: 'Unaudited', variant: 'caution' },
};

/** @type {Record<AuditStatus, StatusBadge | undefined>} */
const NAV_BADGES = {
    audited: undefined,
    outdated: { text: 'Changed', variant: 'caution' },
    unaudited: { text: 'Unaudited', variant: 'caution' },
};

/**
 * @param {Record<string, StatusBadge | undefined>} badges
 * @param {string | undefined} status
 */
const lookup = (badges, status) => {
    const badge =
        status !== undefined && Object.hasOwn(badges, status)
            ? badges[status]
            : undefined;
    return badge ? { ...badge } : undefined;
};

/**
 * Badge for the page title: every computed status is shown.
 *
 * @param {string | undefined} status
 * @returns {StatusBadge | undefined}
 */
export function titleBadge(status) {
    return lookup(TITLE_BADGES, status);
}

/**
 * Badge for navigation: only a status that is not `audited` is shown, so a
 * reader scanning the sidebar sees exactly the pages that still need an audit.
 *
 * @param {string | undefined} status
 * @returns {StatusBadge | undefined}
 */
export function navBadge(status) {
    return lookup(NAV_BADGES, status);
}
