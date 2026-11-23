import { describe, expect, it, vi } from 'vitest';
import {
    FORMS,
    fitForm,
    HYSTERESIS,
    installScript,
    METRICS,
    rowWidth,
} from './nav-fit.mjs';

const REM = 16;
// Guides, Design, Reference: label widths in px.
const LABELS = [44, 46, 68];
const wide = rowWidth(LABELS, METRICS.wide, REM);
const compact = rowWidth(LABELS, METRICS.compact, REM);
const slack = HYSTERESIS * REM;

const formFor = (available, current) =>
    fitForm({ available, textWidths: LABELS, rem: REM, current });

describe('rowWidth', () => {
    it('adds the labels, each link padding and the gaps between links', () => {
        // 158 of labels, 3 × 2 × 12 of padding, 2 × 4 of gap.
        expect(wide).toBe(158 + 72 + 8);
        // 3 × 2 × 8 of padding, 2 × 2 of gap.
        expect(compact).toBe(158 + 48 + 4);
    });

    it('scales the spacing with the root font size', () => {
        expect(rowWidth([10], METRICS.wide, 20)).toBe(10 + 2 * 0.75 * 20);
    });

    it('is zero for no links', () => {
        expect(rowWidth([], METRICS.wide, REM)).toBe(0);
    });
});

describe('fitForm', () => {
    it('lists the forms widest first', () => {
        expect(FORMS).toEqual(['wide', 'compact', 'pill']);
        expect(compact).toBeLessThan(wide);
    });

    it('keeps the row as designed while it fits', () => {
        expect(formFor(wide)).toBe('wide');
        expect(formFor(wide * 3)).toBe('wide');
    });

    it('tightens the row only when the designed row does not fit', () => {
        expect(formFor(wide - 1)).toBe('compact');
        expect(formFor(compact)).toBe('compact');
    });

    it('folds into the pill only when no row fits', () => {
        expect(formFor(compact - 1)).toBe('pill');
        expect(formFor(1)).toBe('pill');
    });

    it('stays in a form while it fits, without the margin', () => {
        expect(formFor(wide, 'wide')).toBe('wide');
        expect(formFor(compact, 'compact')).toBe('compact');
    });

    it('shrinks as soon as the shown form stops fitting', () => {
        expect(formFor(wide - 1, 'wide')).toBe('compact');
        expect(formFor(compact - 1, 'compact')).toBe('pill');
        expect(formFor(compact - 1, 'wide')).toBe('pill');
    });

    it('grows into a wider form only with the margin to spare', () => {
        expect(formFor(wide, 'compact')).toBe('compact');
        expect(formFor(wide + slack - 1, 'compact')).toBe('compact');
        expect(formFor(wide + slack, 'compact')).toBe('wide');
        expect(formFor(compact + slack - 1, 'pill')).toBe('pill');
        expect(formFor(compact + slack, 'pill')).toBe('compact');
        expect(formFor(wide + slack, 'pill')).toBe('wide');
    });

    it('treats an unknown current form as a first measurement', () => {
        expect(formFor(wide, 'bogus')).toBe('wide');
        expect(formFor(wide, null)).toBe('wide');
    });

    it('folds into the pill when nothing is measured yet', () => {
        expect(formFor(0)).toBe('pill');
        expect(fitForm({ available: 500, textWidths: [], rem: REM })).toBe(
            'pill',
        );
    });
});

describe('installScript', () => {
    // The script's whole DOM surface, as stubs: one header element with three
    // links in a box of `width` px.
    function run(width, { current } = {}) {
        const props = new Map();
        const linkProps = [];
        const root = {
            clientWidth: width,
            dataset: current ? { fit: current } : {},
            style: { setProperty: (k, v) => props.set(k, v) },
            querySelectorAll: () =>
                LABELS.map((w, i) => {
                    linkProps[i] = new Map();
                    return {
                        style: {
                            setProperty: (k, v) => linkProps[i].set(k, v),
                        },
                        querySelector: () => ({
                            getBoundingClientRect: () => ({ width: w }),
                        }),
                    };
                }),
            querySelector: () => null,
        };
        const frames = [];
        const env = {
            document: {
                querySelector: () => root,
                documentElement: {},
                fonts: undefined,
            },
            getComputedStyle: () => ({ fontSize: `${REM}px` }),
            requestAnimationFrame: (cb) => frames.push(cb),
            window: {},
        };
        const script = new Function(...Object.keys(env), installScript());
        script(...Object.values(env));
        return { root, props, linkProps, frames };
    }

    it('sets the form and its spacing before the first frame', () => {
        const { root, props, linkProps } = run(compact);
        expect(root.dataset.fit).toBe('compact');
        expect(props.get('--capsule-nav-pad-x')).toBe(
            `${METRICS.compact.padX}rem`,
        );
        expect(props.get('--capsule-nav-gap')).toBe(
            `${METRICS.compact.gap}rem`,
        );
        expect(linkProps[2].get('--capsule-nav-label')).toBe('68px');
        expect(root.dataset.fitReady).toBeUndefined();
    });

    it('enables transitions only after the first frame', () => {
        const { root, frames } = run(wide);
        frames.shift()();
        expect(root.dataset.fitReady).toBeUndefined();
        frames.shift()();
        expect(root.dataset.fitReady).toBe('');
    });

    it('folds into the pill when no row fits', () => {
        expect(run(compact - 1).root.dataset.fit).toBe('pill');
    });

    it('re-fits whenever an observed box resizes', () => {
        const observed = [];
        let callback;
        globalThis.ResizeObserver = class {
            constructor(cb) {
                callback = cb;
            }
            observe(el) {
                observed.push(el);
            }
        };
        const spy = vi.fn();
        try {
            const props = new Map();
            const root = {
                clientWidth: wide,
                dataset: {},
                style: { setProperty: (k, v) => props.set(k, v) },
                querySelectorAll: () =>
                    LABELS.map((w) => ({
                        style: { setProperty: spy },
                        querySelector: () => ({
                            getBoundingClientRect: () => ({ width: w }),
                        }),
                    })),
                querySelector: () => null,
            };
            const env = {
                document: {
                    querySelector: () => root,
                    documentElement: {},
                },
                getComputedStyle: () => ({ fontSize: `${REM}px` }),
                requestAnimationFrame: () => {},
                window: { ResizeObserver: globalThis.ResizeObserver },
            };
            new Function(...Object.keys(env), installScript())(
                ...Object.values(env),
            );
            expect(root.dataset.fit).toBe('wide');
            // The root and each label.
            expect(observed).toHaveLength(1 + LABELS.length);
            root.clientWidth = compact - 1;
            callback();
            expect(root.dataset.fit).toBe('pill');
        } finally {
            delete globalThis.ResizeObserver;
        }
    });

    it('does nothing on a page without the section links', () => {
        const env = {
            document: { querySelector: () => null },
        };
        expect(() =>
            new Function(...Object.keys(env), installScript())(
                ...Object.values(env),
            ),
        ).not.toThrow();
    });
});
