import { describe, expect, it } from 'vitest';
import {
    CYCLE,
    nextTheme,
    parseTheme,
    resolveTheme,
    storedValue,
    toggleLabel,
} from './theme-cycle.mjs';

describe('parseTheme', () => {
    it.each([
        ['light', 'light'],
        ['dark', 'dark'],
        ['auto', 'auto'],
        ['', 'auto'],
        [null, 'auto'],
        [false, 'auto'],
        ['sepia', 'auto'],
    ])('%j -> %s', (value, expected) => {
        expect(parseTheme(value)).toBe(expected);
    });
});

describe('nextTheme', () => {
    it('cycles Auto -> Light -> Dark -> Auto', () => {
        expect(nextTheme('auto')).toBe('light');
        expect(nextTheme('light')).toBe('dark');
        expect(nextTheme('dark')).toBe('auto');
    });

    it('returns to the start after one full cycle from every state', () => {
        for (const start of CYCLE) {
            expect(nextTheme(nextTheme(nextTheme(start)))).toBe(start);
        }
    });
});

describe('resolveTheme', () => {
    it.each([
        ['auto', true, 'light'],
        ['auto', false, 'dark'],
        ['light', false, 'light'],
        ['dark', true, 'dark'],
    ])('%s with system light=%s -> %s', (pref, systemLight, expected) => {
        expect(resolveTheme(pref, systemLight)).toBe(expected);
    });
});

describe('storedValue', () => {
    it('stores auto as the empty string, as Starlight does', () => {
        expect(storedValue('auto')).toBe('');
        expect(storedValue('light')).toBe('light');
        expect(storedValue('dark')).toBe('dark');
    });

    it('round-trips through parseTheme', () => {
        for (const pref of CYCLE) {
            expect(parseTheme(storedValue(pref))).toBe(pref);
        }
    });
});

describe('toggleLabel', () => {
    it('names the current preference and the next one', () => {
        expect(toggleLabel('auto')).toBe('Theme: Auto. Switch to Light.');
        expect(toggleLabel('dark')).toBe('Theme: Dark. Switch to Auto.');
    });
});
