// The direction of a cross-document view transition. A navigation back
// through history plays the page transition in reverse (src/styles/motion.css
// keys the reversed keyframes off the `back` transition type). The decision
// is a pure function of the Navigation API's activation, so it is tested here
// rather than in a browser; `installScript` is the same logic as a classic
// inline script for the page <head>, which must run before the new page's
// first render, when `pagereveal` fires.

/** The view-transition type a backward history step adds. */
export const BACK_TYPE = 'back';

/**
 * The view-transition types for the navigation that revealed this page:
 * `['back']` for a step back through history, otherwise none.
 *
 * @param {{
 *   navigationType?: string,
 *   entry?: { index: number } | null,
 *   from?: { index: number } | null,
 * } | null | undefined} activation `navigation.activation`
 * @returns {string[]}
 */
export function transitionTypes(activation) {
    if (activation?.navigationType !== 'traverse') return [];
    const to = activation.entry?.index;
    const from = activation.from?.index;
    if (typeof to !== 'number' || typeof from !== 'number') return [];
    // No `from` (a cross-origin arrival) or a `from` of -1 (an entry no
    // longer in the list) has no direction to reverse.
    if (from < 0 || to >= from) return [];
    // A literal, not BACK_TYPE: installScript ships this function's source,
    // which must not refer to anything outside it.
    return ['back'];
}

/**
 * Inline <head> script: on `pagereveal`, tags the incoming view transition
 * with `transitionTypes`. A no-op in browsers without the event, the
 * Navigation API or a transition in progress.
 *
 * @returns {string}
 */
export function installScript() {
    return `(() => {
const transitionTypes = ${transitionTypes.toString()};
window.addEventListener('pagereveal', (event) => {
    const transition = event.viewTransition;
    if (!transition || !transition.types || !window.navigation) return;
    for (const type of transitionTypes(window.navigation.activation)) {
        transition.types.add(type);
    }
});
})();`;
}
