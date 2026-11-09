// The single mapping from a page's review `status` (see src/content.config.ts)
// to the badge that shows it. The page title and the sidebar both read it, so
// the two can never disagree.

/** @typedef {'draft' | 'stable'} ReviewStatus */
/** @typedef {{ text: string, variant: 'success' | 'caution' }} StatusBadge */

/** @type {Record<ReviewStatus, StatusBadge>} */
const BADGES = {
    stable: { text: 'Stable', variant: 'success' },
    draft: { text: 'Draft', variant: 'caution' },
};

/**
 * Badge for the page title: every known status is shown.
 *
 * @param {string | undefined} status
 * @returns {StatusBadge | undefined}
 */
export function titleBadge(status) {
    return status !== undefined && Object.hasOwn(BADGES, status)
        ? { ...BADGES[/** @type {ReviewStatus} */ (status)] }
        : undefined;
}

/**
 * Badge for navigation: only a status that is not `stable` is shown, so a
 * reader scanning the sidebar sees exactly the pages still awaiting review.
 *
 * @param {string | undefined} status
 * @returns {StatusBadge | undefined}
 */
export function navBadge(status) {
    return status === 'stable' ? undefined : titleBadge(status);
}
