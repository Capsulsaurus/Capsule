import { describe, expect, it } from 'vitest';
import { localizeHero } from './hero-links.mjs';

const hero = Object.freeze({
    title: 'T',
    actions: Object.freeze([
        Object.freeze({ text: 'Start', link: '/guides/getting-started/' }),
        Object.freeze({ text: 'Code', link: 'https://github.com/x' }),
        Object.freeze({ text: 'Already', link: '/ja/design/' }),
    ]),
});

describe('localizeHero', () => {
    it("keeps the actions' site links in the locale, without touching the input", () => {
        expect(localizeHero(hero, 'ja')).toEqual({
            title: 'T',
            actions: [
                { text: 'Start', link: '/ja/guides/getting-started/' },
                { text: 'Code', link: 'https://github.com/x' },
                { text: 'Already', link: '/ja/design/' },
            ],
        });
        expect(hero.actions[0].link).toBe('/guides/getting-started/');
    });

    it('returns the same hero when there is nothing to change', () => {
        expect(localizeHero(hero, undefined)).toBe(hero);
        expect(localizeHero(undefined, 'ja')).toBeUndefined();
        const bare = { title: 'T' };
        expect(localizeHero(bare, 'ja')).toBe(bare);
    });
});
