// The site chrome's own words in every docs locale: the sidebar's top-level
// group labels, the header's section links and the footer's jump links. The
// pages themselves are translated in src/content/docs/<key>/ and Starlight's
// UI strings come from Starlight (or src/content/i18n/); these are the few
// labels this site adds. English is the key and the fallback.

import { DOCS_LOCALES, langOf } from './locales.mjs';

/**
 * Each English label's rendering per BCP-47 tag. Every label carries every
 * docs locale (src/lib/nav-labels.test.mjs).
 *
 * @type {Readonly<Record<string, Readonly<Record<string, string>>>>}
 */
export const NAV_LABELS = Object.freeze({
    Guides: {
        'zh-Hans': '指南',
        'zh-Hant': '指南',
        ja: 'ガイド',
        ko: '가이드',
        fr: 'Guides',
        de: 'Anleitungen',
        es: 'Guías',
        'pt-BR': 'Guias',
        it: 'Guide',
        ru: 'Руководства',
        hi: 'गाइड',
        ar: 'الأدلة',
    },
    Features: {
        'zh-Hans': '功能',
        'zh-Hant': '功能',
        ja: '機能',
        ko: '기능',
        fr: 'Fonctionnalités',
        de: 'Funktionen',
        es: 'Funciones',
        'pt-BR': 'Recursos',
        it: 'Funzionalità',
        ru: 'Возможности',
        hi: 'सुविधाएँ',
        ar: 'الميزات',
    },
    Design: {
        'zh-Hans': '设计',
        'zh-Hant': '設計',
        ja: '設計',
        ko: '설계',
        fr: 'Conception',
        de: 'Design',
        es: 'Diseño',
        'pt-BR': 'Design',
        it: 'Progettazione',
        ru: 'Архитектура',
        hi: 'डिज़ाइन',
        ar: 'التصميم',
    },
    Development: {
        'zh-Hans': '开发',
        'zh-Hant': '開發',
        ja: '開発',
        ko: '개발',
        fr: 'Développement',
        de: 'Entwicklung',
        es: 'Desarrollo',
        'pt-BR': 'Desenvolvimento',
        it: 'Sviluppo',
        ru: 'Разработка',
        hi: 'विकास',
        ar: 'التطوير',
    },
    Reference: {
        'zh-Hans': '参考',
        'zh-Hant': '參考',
        ja: 'リファレンス',
        ko: '레퍼런스',
        fr: 'Référence',
        de: 'Referenz',
        es: 'Referencia',
        'pt-BR': 'Referência',
        it: 'Riferimento',
        ru: 'Справочник',
        hi: 'संदर्भ',
        ar: 'المرجع',
    },
    Blog: {
        'zh-Hans': '博客',
        'zh-Hant': '部落格',
        ja: 'ブログ',
        ko: '블로그',
        fr: 'Blog',
        de: 'Blog',
        es: 'Blog',
        'pt-BR': 'Blog',
        it: 'Blog',
        ru: 'Блог',
        hi: 'ब्लॉग',
        ar: 'المدونة',
    },
    // The narrow-screen section menu's name outside every section.
    Sections: {
        'zh-Hans': '栏目',
        'zh-Hant': '欄目',
        ja: 'セクション',
        ko: '섹션',
        fr: 'Rubriques',
        de: 'Bereiche',
        es: 'Secciones',
        'pt-BR': 'Seções',
        it: 'Sezioni',
        ru: 'Разделы',
        hi: 'अनुभाग',
        ar: 'الأقسام',
    },
    Documentation: {
        'zh-Hans': '文档',
        'zh-Hant': '文件',
        ja: 'ドキュメント',
        ko: '문서',
        fr: 'Documentation',
        de: 'Dokumentation',
        es: 'Documentación',
        'pt-BR': 'Documentação',
        it: 'Documentazione',
        ru: 'Документация',
        hi: 'दस्तावेज़',
        ar: 'التوثيق',
    },
    Roadmap: {
        'zh-Hans': '路线图',
        'zh-Hant': '路線圖',
        ja: 'ロードマップ',
        ko: '로드맵',
        fr: 'Feuille de route',
        de: 'Roadmap',
        es: 'Hoja de ruta',
        'pt-BR': 'Roteiro',
        it: 'Roadmap',
        ru: 'Дорожная карта',
        hi: 'रोडमैप',
        ar: 'خارطة الطريق',
    },
    Discussions: {
        'zh-Hans': '讨论',
        'zh-Hant': '討論',
        ja: 'ディスカッション',
        ko: '토론',
        fr: 'Discussions',
        de: 'Diskussionen',
        es: 'Debates',
        'pt-BR': 'Discussões',
        it: 'Discussioni',
        ru: 'Обсуждения',
        hi: 'चर्चाएँ',
        ar: 'النقاشات',
    },
    Support: {
        'zh-Hans': '支持',
        'zh-Hant': '支援',
        ja: 'サポート',
        ko: '지원',
        fr: 'Assistance',
        de: 'Support',
        es: 'Soporte',
        'pt-BR': 'Suporte',
        it: 'Supporto',
        ru: 'Поддержка',
        hi: 'सहायता',
        ar: 'الدعم',
    },
});

/**
 * `label` in a locale's language, or `label` itself (English) for the root
 * locale or a label with no translation (a proper name such as GitHub).
 *
 * @param {string} label The English label.
 * @param {string | undefined} locale A docs locale key, or undefined for root.
 * @returns {string}
 */
export function localizeLabel(label, locale) {
    if (locale === undefined || locale === 'root') return label;
    return NAV_LABELS[label]?.[langOf(locale)] ?? label;
}

/**
 * A Starlight sidebar entry's `translations` for an English label: every
 * docs locale's rendering, keyed by BCP-47 tag.
 *
 * @param {string} label
 * @returns {Record<string, string>}
 */
export function sidebarTranslations(label) {
    const byLang = NAV_LABELS[label];
    if (!byLang) throw new Error(`no translations for the label "${label}"`);
    return Object.fromEntries(
        DOCS_LOCALES.map(({ lang }) => [lang, byLang[lang]]),
    );
}
