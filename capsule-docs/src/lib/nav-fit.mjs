// Which form the header's section links take: `wide` (the row as designed),
// `compact` (the same row, tighter) or `pill` (one pill naming the current
// section that opens the list). The choice is the widest form whose row fits
// the free space between the wordmark and the search field, so the links fold
// only when they would otherwise collide with a neighbour, never at a fixed
// window width. The decision is a pure function of measured widths, tested
// here; `installScript` is the same logic as a classic inline script that
// measures the header, sets the form before the first paint and follows every
// resize after it (Header.astro). The contract is
// development/visual-language.md (Docs Site, Header).

/** The forms, widest first. */
export const FORMS = /** @type {const} */ (['wide', 'compact', 'pill']);

/**
 * Spacing of the row forms, in rem: each link's inline padding and the gap
 * between links. The script writes them onto the header as custom properties,
 * so the CSS that animates between forms and this arithmetic read one source.
 *
 * @type {Record<'wide' | 'compact', { padX: number, gap: number }>}
 */
export const METRICS = {
    wide: { padX: 0.75, gap: 0.25 },
    compact: { padX: 0.5, gap: 0.125 },
};

/**
 * How much more room than it needs a wider form must have before the links
 * grow into it, in rem. Without it a window resting on a boundary would flip
 * between forms on every sub-pixel of layout.
 */
export const HYSTERESIS = 0.5;

/**
 * The width a row of links takes, in px.
 *
 * @param {number[]} textWidths each link's label width, in px
 * @param {{ padX: number, gap: number }} metrics in rem
 * @param {number} rem the root font size, in px
 * @returns {number}
 */
export function rowWidth(textWidths, metrics, rem) {
    if (textWidths.length === 0) return 0;
    const labels = textWidths.reduce((sum, width) => sum + width, 0);
    const padding = textWidths.length * 2 * metrics.padX * rem;
    const gaps = (textWidths.length - 1) * metrics.gap * rem;
    return labels + padding + gaps;
}

/**
 * The widest form whose row fits `available`. A form wider than `current`
 * must fit with `HYSTERESIS` to spare; `current` itself, or a narrower form,
 * need only fit. When no row fits, the links fold into the pill.
 *
 * @param {{
 *   available: number,
 *   textWidths: number[],
 *   rem: number,
 *   current?: string | null,
 * }} input widths in px; `current` is the form shown now, if any
 * @returns {'wide' | 'compact' | 'pill'}
 */
export function fitForm({ available, textWidths, rem, current }) {
    // installScript ships this function's source beside FORMS, METRICS,
    // HYSTERESIS and rowWidth, so it may refer to those and nothing else.
    const shown = FORMS.indexOf(/** @type {any} */ (current));
    if (textWidths.length > 0 && available > 0) {
        for (const form of /** @type {const} */ (['wide', 'compact'])) {
            const needed = rowWidth(textWidths, METRICS[form], rem);
            const grows = shown !== -1 && FORMS.indexOf(form) < shown;
            if (available >= needed + (grows ? HYSTERESIS * rem : 0)) {
                return form;
            }
        }
    }
    return 'pill';
}

/**
 * Inline script, placed after the header's markup: sets `data-fit` on the
 * `[data-section-nav]` element and its spacing custom properties, then keeps
 * them current as the header, the labels (a web font arriving) or the window
 * change. Transitions are enabled (`data-fit-ready`) only after the first
 * frame, so a page opens in its form rather than animating into it. A no-op
 * without the element; without ResizeObserver the form is set once.
 *
 * @returns {string}
 */
export function installScript() {
    return `(() => {
const FORMS = ${JSON.stringify(FORMS)};
const METRICS = ${JSON.stringify(METRICS)};
const HYSTERESIS = ${JSON.stringify(HYSTERESIS)};
const rowWidth = ${rowWidth.toString()};
const fitForm = ${fitForm.toString()};
const root = document.querySelector('[data-section-nav]');
if (!root) return;
const links = [...root.querySelectorAll('.site-nav.bar a')];
const labels = links.map((link) => link.querySelector('.label') || link);
const fit = () => {
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    const textWidths = labels.map((label) => label.getBoundingClientRect().width);
    labels.forEach((label, i) => {
        links[i].style.setProperty('--capsule-nav-label', textWidths[i] + 'px');
    });
    const form = fitForm({ available: root.clientWidth, textWidths, rem, current: root.dataset.fit });
    const metrics = METRICS[form] || METRICS.compact;
    root.style.setProperty('--capsule-nav-pad-x', metrics.padX + 'rem');
    root.style.setProperty('--capsule-nav-gap', metrics.gap + 'rem');
    if (root.dataset.fit === form) return;
    root.dataset.fit = form;
    if (form !== 'pill') {
        const open = root.querySelector('[popover]');
        if (open && open.matches(':popover-open')) open.hidePopover();
    }
};
fit();
if (window.ResizeObserver) {
    const observer = new ResizeObserver(fit);
    observer.observe(root);
    for (const label of labels) observer.observe(label);
}
if (document.fonts) document.fonts.ready.then(fit);
requestAnimationFrame(() => requestAnimationFrame(() => {
    root.dataset.fitReady = '';
}));
})();`;
}
