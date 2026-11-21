import { describe, expect, it } from 'vitest';
import { DOCS_LOCALES } from './locales.mjs';
import {
    localizeLabel,
    NAV_LABELS,
    sidebarTranslations,
} from './nav-labels.mjs';

describe('NAV_LABELS', () => {
    it.each(
        Object.keys(NAV_LABELS),
    )('renders %s in every docs locale', (label) => {
        const byLang = NAV_LABELS[label];
        expect(Object.keys(byLang).sort()).toEqual(
            DOCS_LOCALES.map(({ lang }) => lang).sort(),
        );
        for (const text of Object.values(byLang)) {
            expect(text.trim()).not.toBe('');
        }
    });

    it('covers the sidebar groups and the header and footer links', () => {
        for (const label of [
            'Guides',
            'Features',
            'Design',
            'Development',
            'Reference',
            'Blog',
            'Sections',
            'Documentation',
            'Roadmap',
            'Discussions',
            'Support',
        ]) {
            expect(NAV_LABELS).toHaveProperty(label);
        }
    });
});

describe('localizeLabel', () => {
    it('translates by locale key and keeps English for the root', () => {
        expect(localizeLabel('Guides', 'ja')).toBe('ガイド');
        expect(localizeLabel('Guides', 'pt-br')).toBe('Guias');
        expect(localizeLabel('Guides', undefined)).toBe('Guides');
        expect(localizeLabel('Guides', 'root')).toBe('Guides');
    });

    it('keeps a label with no translation, such as a proper name', () => {
        expect(localizeLabel('GitHub', 'ja')).toBe('GitHub');
    });
});

describe('sidebarTranslations', () => {
    it('keys every locale by its BCP-47 tag, as Starlight reads them', () => {
        const translations = sidebarTranslations('Design');
        expect(translations['zh-Hans']).toBe('设计');
        expect(translations['zh-Hant']).toBe('設計');
        expect(Object.keys(translations)).toHaveLength(DOCS_LOCALES.length);
    });

    it('refuses a label it has no translations for', () => {
        expect(() => sidebarTranslations('Nope')).toThrow(/no translations/);
    });
});
