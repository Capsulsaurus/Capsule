import { describe, expect, it } from 'vitest';
import {
    BACK_TYPE,
    installScript,
    transitionTypes,
} from './page-transition.mjs';

const traverse = (from, to) => ({
    navigationType: 'traverse',
    from: { index: from },
    entry: { index: to },
});

describe('transitionTypes', () => {
    it('marks a step back through history', () => {
        expect(transitionTypes(traverse(3, 2))).toEqual([BACK_TYPE]);
        expect(transitionTypes(traverse(5, 0))).toEqual([BACK_TYPE]);
    });

    it('leaves a step forward through history unmarked', () => {
        expect(transitionTypes(traverse(2, 3))).toEqual([]);
    });

    it.each([
        'push',
        'replace',
        'reload',
    ])('leaves a %s navigation unmarked', (navigationType) => {
        expect(transitionTypes({ ...traverse(3, 2), navigationType })).toEqual(
            [],
        );
    });

    it('leaves an arrival from another origin unmarked', () => {
        expect(transitionTypes(traverse(-1, 0))).toEqual([]);
    });

    it.each([
        [null],
        [undefined],
        [{}],
        [{ navigationType: 'traverse', entry: { index: 1 }, from: null }],
        [{ navigationType: 'traverse', entry: null, from: { index: 2 } }],
    ])('leaves an incomplete activation unmarked: %j', (activation) => {
        expect(transitionTypes(activation)).toEqual([]);
    });
});

describe('installScript', () => {
    // Runs the inline script against a stub window and fires `pagereveal`.
    function reveal({ activation, viewTransition, hasNavigation = true }) {
        let listener;
        const window = {
            navigation: hasNavigation ? { activation } : undefined,
            addEventListener: (name, fn) => {
                expect(name).toBe('pagereveal');
                listener = fn;
            },
        };
        new Function('window', installScript())(window);
        listener({ viewTransition });
    }

    it('adds the back type on a backward traverse', () => {
        const types = new Set();
        reveal({ activation: traverse(4, 3), viewTransition: { types } });
        expect([...types]).toEqual([BACK_TYPE]);
    });

    it('adds nothing on a forward navigation', () => {
        const types = new Set();
        reveal({
            activation: { ...traverse(1, 2), navigationType: 'push' },
            viewTransition: { types },
        });
        expect([...types]).toEqual([]);
    });

    it('does nothing without a view transition', () => {
        expect(() =>
            reveal({ activation: traverse(4, 3), viewTransition: null }),
        ).not.toThrow();
    });

    it('does nothing without the Navigation API', () => {
        const types = new Set();
        reveal({
            activation: traverse(4, 3),
            viewTransition: { types },
            hasNavigation: false,
        });
        expect([...types]).toEqual([]);
    });
});
